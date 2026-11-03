<p align="right">
  <a href="README.md">English</a> · <strong>简体中文</strong>
</p>

# VibeMic(feature/vibemic 分支)

VibeMic 把 FoloToy AI Passport(ESP32-C3)变成 vibecoding 语音遥控器:一台 BLE
HID 键盘,三个按键驱动 PC 上当前焦点 AI 工具。按住 OK 时设备麦克风经 BLE 把音频
串流给配套桌面软件(工作区目录 `../desktop`),由它在本地识别后把文字键入焦点窗口。

## 本分支构建什么

| 按键 | 按下 / 抬起 | 短按 | 双击 | 长按 |
| --- | --- | --- | --- | --- |
| OK | PTT:按住说话(按下 = 开始麦克风串流,抬起 = 停止 → PC 识别并键入);仅 PC 会话活跃时生效。**双击的第二下**是桌面端的删除手势:不启动 PTT(麦克风不开),屏上显示 `DELETE` | 忽略 | 删除手势(仅第二下) | 忽略 |
| UP | — | `Enter`(发送) | 预留(空槽) | `Ctrl+Enter` |
| DOWN | — | `Esc`(取消) | 两记 `Esc`(停止:如 DSH 双击 Esc 中断当前运行) | 按住 10 秒 = **蓝牙硬重置**:清空配对、断开链路、重新广播(恢复手势,由设备本地执行,PC 软件离线也能用;之后需在主机上删除配对设备再重新配对) |

按键组件只在第二下按下落在第一下抬起后的 180ms 窗口内时上报双击,并且会**丢掉**那次
待发的单击而不是补发;单击与双击是不同槽位,所以快按两下不会退化成一次单击。设备侧
`vm_keymap_is_double_press()` 用同一个 180ms 窗口识别 OK 的删除手势。

- BLE HID 键盘(HOGP,支持 boot/report 协议),Just-Works 配对绑定。
- VibeMic 自定义 GATT 服务(Base UUID `a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7aXX`),
  承载分帧控制协议(见 `main/vm_protocol.c`)。
- 内置三套 profile(Trae 切换式 / 微信按住式 / Win+H 听写);激活态持久化到
  NVS,可由 PC 端切换。
- 全新设计的单屏 UI(产品名、连接状态、电量、PTT 计时)。

## 桌面会话兼容性

固件 `0x0006` 要求 STATUS 和 EVENT 订阅就绪后,通过加密 VM_CTRL 写入
`CMD_PC_SESSION`(`0x05`)。载荷为一个字节:`1` 续租 15 秒,`0` 结束会话。
桌面链路每 5 秒续租,录音期间也续租。残留 CCCD 不再激活 PC 模式;
租约过期后恢复直接 HID 并停止麦克风串流。VM_PROFILE 只接受 PROFILE_CHUNK,
两个可写特征值都要求加密。

请先更新桌面软件再烧录此固件。新版桌面端兼容旧固件;旧桌面端配合此固件
只能使用设备独立 HID,不能启用桌面按键分发或录音。

## 目录

```
main/
  vibemic_app.c   应用入口、主屏 UI(全新设计)、状态轮询
  vm_buttons.c    按键事件 → profile 槽位解析 → HID 宏队列
  vm_hid.c        NimBLE 栈:HID/电池/设备信息服务、广播、连接管理、
                  宏执行器(报文 10ms 节流)
  vm_link.c       VibeMic 自定义服务:帧解码、命令工作任务、通知
  vm_profile.c    内置 profile + NVS 激活态持久化
  vm_protocol.c   协议帧编解码(纯 C,host tests:tests/test_vm_protocol.c)
components/bsp/   仅 bsp_button 新增 BSP_BTN_RELEASE 事件,其余不变
```

基线 demo 源码保留在 `main/` 但不参与编译,上游 host tests 照常可用;本应用
不复用 demo 菜单与其视觉外壳。

## 构建与测试

```bash
./tools/validate.sh --static    # 仓库检查 + host tests(含 vm_protocol)
./tools/validate.sh --firmware  # ESP-IDF 5.5.3 构建 + 合并镜像
```

当前状态:上述固件与协议/控制面模块已实现。profile 分块上传、设备端菜单、
双主机 bond 槽尚未实现。
