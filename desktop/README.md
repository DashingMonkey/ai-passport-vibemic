# desktop/ —— PC 端桌面软件

Windows 10/11 的语音输入伴侣。设备被系统当作 HID 键盘持续连接、不广播，
桌面端按设备名经 WinRT 直连自定义 GATT 服务。托盘常驻，
**按住设备 OK 说话 → 本机识别（SenseVoice，离线）→ 文字键入当前焦点窗口**。

连续口述时，若 Windows 辅助功能接口能确认光标前已有普通文字，下一次识别结果前会自动补 `。`。
读取范围限制在当前输入框内；清空后残留的零宽字符不算文字。空输入框、换行后、已有标点或正在替换选中文字时不补。系统或输入框不支持读取、读取失败或超过
200 毫秒时静默跳过，照常输入识别结果；不靠上次识别内容猜测，不读取密码框，也不保存读取到的文字。
`%APPDATA%/vibemic/app.log` 中的 `[punctuation]` 日志记录补句号或跳过的原因，不包含读取到的输入框文字。

界面五页：语音识别 / 按键 / 固件烧录 / 设置 / 关于；设备连接状态与电量常显在标题栏，
开机自启动与检查更新分别在设置页与关于页。

## 快速开始

```bash
cargo build --workspace     # → target/debug/{vmctl.exe, VibeMic.exe}
cargo test --workspace      # 36 例单测

# 设备先在系统蓝牙设置里配对（它是一台 HID 键盘），然后：
vmctl scan                  # 扫描 BLE + 列串口（Windows 上看不到已配对的 VibeMic）
vmctl status                # WinRT 直连 → GET_STATUS → 打印设备状态
vmctl set-active <id>       # 切换设备内置快捷键档（0=Trae 1=WeChat 2=Win Dict）
vmctl watch                 # 持续打印上行帧：按设备按键看 BUTTON_EVENT
vmctl record out.wav        # 按住 OK 录一段，落盘 WAV（语音链路验收工具）
```

## Workspace 结构

| crate | 职责 |
| --- | --- |
| `crates/vibemic-protocol` | 协议帧编解码(与设备端 `vm_protocol.c` 同一组测试向量约束)、STATUS/RESULT/BUTTON_EVENT 载荷、ADPCM 解码、profile JSON 模型、HID 键码表、UUID 常量 |
| `crates/vibemic-link` | `DeviceLink` trait;`WinRtLink`(Windows 正式路径,按名枚举已配对设备)+ `BleLink`(btleplug,广播场景/扫描)+ `SerialLink`(serialport);`scan_ble` |
| `crates/vibemic-asr` | sherpa-onnx SenseVoice-Small 封装 + 模型下载/校验(读超时、content-length 比对、`.part` 清理、强制重下) |
| `crates/vmctl` | 验证 CLI(上方命令表) |
| `app/` | Tauri 2 + Vue 3 桌面应用(托盘 + 悬浮窗 + 配置页) |

## 构建桌面应用

一键(依赖检查 → 首次自动下载 sherpa-onnx 引擎归档约 117MB 到 `third_party/` ——
下完会校验完整性,中断留下的残包自动删掉重下;若那里已有解压好的 `lib/` 就直接复用、
不再下载 → 关掉运行中的实例 → 前端类型检查 → release 构建 → 启动):

```bat
desktop\build.bat
```

手动方式:

```bash
cd app
npm install
npm run tauri dev        # 开发
npm run tauri build      # 产出双击可用的 exe(release)
```

产物为 `target/release/VibeMic.exe`。
**这一个文件就是完整交付物**:设备链路同进程,不需要任何 sidecar exe。
若将来要出安装包,把 `tauri.conf.json` 的 `bundle.active` 改成 true 即可
(当前没有额外二进制需要打包)。

构建脚本跑完会**直接把配置面板打开**(正常启动是不弹面板、只留托盘的,刚构建完看不到
任何窗口会让人以为没成功)。面板关掉只是收回托盘;**退出走托盘图标的右键菜单**。
软件同时只允许跑一份:再双击一次 exe 不会起第二个进程,而是把已经开着的面板调到前台。

## 固件烧录

设备固件可以在软件里直接烧,不用另装 esptool。

**官方交付方式:只烧合并镜像,从 `0x0` 起写。** 面板上用「选择 .bin…」挑
`FoloToy-AI-Passport-full.bin`;不挑也行 —— 软件会**在它自己所在目录里找**:
先看 `firmware/` 子目录(交付包的摆法),再看 exe 同目录(散装摆法),先命中的那条生效。
手动挑过之后,就严格只用你挑的那个文件;就算它被删了也照报错,不会悄悄换成别的镜像去烧。

写之前会验证 ESP32-C3 bootloader 和应用的完整性、`0x8000` 分区表的 MD5、分区边界及重叠,
并显示整份文件的 SHA-256 供你比对。当前支持含 factory 应用的标准合并布局;app-only、截断或损坏的文件会被拒绝。串口默认选带 `USB-JTAG` 标记的那个(这类设备自带下载模式,不用按 BOOT);
写后回读校验默认开着,慢一点但能挡住"写了一半"。波特率默认 460800。

> ⚠️ **烧录会清空设备数据**:设备端的快捷键档回默认(Trae),而且**蓝牙要重新配对一次**
> —— 合并镜像覆盖了 NVS,设备侧绑定没了、Windows 这边还留着旧密钥,不重新配对会连不上。
> 面板在写入按钮下方用红色提示条写明这一点,右侧就是「打开蓝牙设置」。
> 重新配对时请**先删除系统里那条旧的蓝牙设备,再重新添加**。

## 设备会话与版本配合

固件 `0x0006` 起要求桌面端发送加密的应用会话心跳:订阅全部完成后激活,每 5 秒续租,
15 秒未续租自动恢复设备直接发送 HID 按键。软件退出会撤销远端通知订阅;进程崩溃或蓝牙清理失败也会由租约超时恢复。
升级时先更新桌面软件再更新固件。新版桌面端兼容旧固件;旧桌面端搭配新固件只保留设备裸 HID 操作,不启用桌面按键分发和录音。

## 更多

改代码前先读 [`AGENTS.md`](AGENTS.md):构建/验证清单、硬约束(插件权限、命令必须
`async + spawn_blocking`、锁不许重入…)、托盘/悬浮窗/按键的行为契约、烧录实现细节。
