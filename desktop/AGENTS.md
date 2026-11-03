# AGENTS.md —— desktop/ 子树(PC 端桌面软件)

本文件是 `desktop/` 的权威规则:**改这个子树里的任何东西之前先读这里**。
产品是什么、怎么用、怎么构建看 [`README.md`](README.md);本文件只放
"改代码时必须知道、但用户不需要看"的东西(约束、坑、行为契约、未实现清单)。

> 仓库根 [`../AGENTS.md`](../AGENTS.md) 只做路由。**`../device/` 的规则不适用于本子树**:
> 那边的 ESP-IDF/LVGL/烧录约定与 `passport-*` skill 一律不要带进来。
> 这里唯一的"烧录"是桌面端 app 内的 espflash 功能(见下)。

## 构建与验证

```bash
cargo build --workspace     # → target/debug/{vmctl.exe, VibeMic.exe}
cargo test --workspace      # 36 例:协议 / ADPCM / 设备名兼容 / 配置状态 / 截断判定 /
                            # 链路判活 / 双击判定 / 烧录校验 / 版本比较(检查更新)
cargo check -p vibemic-app  # 只验 app crate(改 Tauri 侧时最快的一圈)

cd app
npm run typecheck           # vue-tsc,前端必过
npm run build               # 前端产物 → app/dist(相对路径,file:// 也能打开)
npm run tauri dev           # 开发(需要能绑定 127.0.0.1:1420)
```

一键构建(依赖检查 → 首次下载 sherpa-onnx 引擎归档约 117MB 到 `third_party/` →
关掉运行中的实例 → **等旧 exe 的文件锁放开** → 前端类型检查 → release 构建 → 启动并唤出面板):
`desktop\build.bat`。

交付并启动独立 exe 必须走 `build.bat` 或 `npm run tauri build`。普通 `cargo build --release`
只保证 Rust 编译；未开启 Tauri 的 `custom-protocol` 时仍会访问开发服务器，导致“无法访问此页面”。

`build.bat` 有几个坑是踩过之后加的,**改它之前先看这几条**:

- **编码必须是 GBK(系统 OEM 码页 936)、CRLF、不带 BOM**。cmd 执行批处理时按**当前控制台
  码页**读文件,不是 UTF-8:文件存成 UTF-8 的话中文注释在命令行窗口里是乱码,而码页切到
  65001 又会让 `for /f` 解析外部命令输出出错(`Cargo.lock` 的版本号就是这么读的,直接
  报 "Cannot read sherpa-onnx-sys version")。所以**保持 936 + GBK**,不要加 `chcp 65001`。
  用编辑工具改完要重新转码(编辑器多半按 UTF-8 保存):
  ```powershell
  $p = "desktop\build.bat"
  $t = [System.IO.File]::ReadAllText($p, (New-Object System.Text.UTF8Encoding($false)))
  $t = $t -replace "`r`n","`n" -replace "`n","`r`n"
  [System.IO.File]::WriteAllBytes($p, [System.Text.Encoding]::GetEncoding(936).GetBytes($t))
  ```
  同理别存成裸 LF:cmd 会一行都不执行,只报 "The system cannot find the path specified"。
- **`if (...)` 块内的 `echo` 不能出现右括号**:cmd 会在那个括号处提前结束块,报
  "was unexpected at this time"。要解释"范围"就写成 `10s`/`10 秒`(如 `after %WAITED%s`),
  不要写 `(约 10s)`。
- **必须等 exe 的文件锁放开**,不能只 `taskkill` 就往下走:exe 刚跑过时 Windows 还占着
  映像几秒,这期间 cargo 链接会以 `failed to remove ... 拒绝访问` 收场,整轮 release 构建
  白跑。判据是"能不能删掉"(只读检测看不出独占锁),删完 cargo 会重新生成。
- **启动时要带 `--show-panel`**:软件平时是"托盘常驻、面板不弹",构建脚本不改这个默认行为,
  只是这一次把面板调出来,否则用户看不到任何窗口、以为没构建成功。
- **引擎归档必须先校验再用**:脚本下完归档做两层校验——体积下限(挡住几百 KB 的残包)
  + `tar -tjf` 能否列出目录(bz2 流完整、尾部索引可读);不通过就删掉重下,并把"该删哪个
  文件、去哪儿手工下"写进报错。**别退回"文件存在就算下好了"的写法**:下载中断留下的
  半个 `.tar.bz2` 会让 `sherpa-onnx-sys` 的 build script 在解压时报
  `failed to unpack ... 某个 .lib`,真因完全看不出来,而且之后每次构建都在同一处失败。
  同样**别把期望字节数钉死**:归档名里的版本号是从 `Cargo.lock` 动态读的,升级
  sherpa-onnx 后体积会变,钉死会让脚本陷入"删掉重下还是不过"的死循环。另外下载**不要
  用 `-C -`**:残包已在上面删掉,带着断点续传只会把上一次的坏前缀留在文件里。
- **离线通道优先于下载**:`third_party\sherpa-onnx\` 下若已有解压好的预编译库
  (`lib\sherpa-onnx-core.lib`,或 `<归档名去掉 .tar.bz2>\lib\sherpa-onnx-core.lib`),
  脚本就设 `SHERPA_ONNX_LIB_DIR` 并跳过归档与下载(sherpa-onnx-sys 的 build script
  最高优先级看这个变量)。弱网机器最省事的做法:从能构建成功的那台把
  `target\sherpa-onnx-prebuilt\sherpa-onnx-v*-lib\lib` 拷过去。

**交付物只有一个 `target/release/VibeMic.exe`**(exe 名由 `app/src-tauri/Cargo.toml` 的
`[[bin]] name = "VibeMic"` 决定;Tauri 2 的 `productName` 只改安装器名)。
设备链路同进程,没有 sidecar。

## 硬约束(踩过坑的,别改回去)

- **Tauri 两侧版本必须同一 major.minor**:Rust 的 `tauri` crate 与 npm 的
  `@tauri-apps/api` 不一致时 `tauri build` 会直接中止("Found version mismatched
  Tauri packages")。因此 `@tauri-apps/api` 用 `~` 锁 minor,`tauri` 由 `Cargo.lock` 锁。
  加插件注意版本下限:如 `tauri-plugin-dialog 2.8` 要求 `tauri = "2.12"`,
  `cargo add` 会顺带抬 `tauri`,JS 侧不同步就构建失败。
- **新插件要同时改三处**:`app/src-tauri/Cargo.toml`(crate)、`app/package.json`(JS 包)、
  `app/src-tauri/capabilities/default.json`(**权限**)。漏了权限,前端调用会被直接拒绝,
  而且报错信息不指向权限名。
- **`tauri-plugin-single-instance` 必须注册在 builder 最前面**:它用本地 socket 抢名字,
  晚了第二个实例会把 dialog/autostart 等插件白初始化一轮再退出。
- **碰磁盘/网络的 `#[tauri::command]` 一律 `async` + `spawn_blocking`**:非 async 命令由
  tauri-macros 在 IPC 回调里原地执行,Windows 上那就是 WebView2 的 UI 线程,阻塞它 =
  整个界面卡死(消息泵停摆)。
- **`Shared` 的锁不许重入**:`state` / `widget` 两把锁,`save_config` 内部还要再取一次;
  持锁调 `save_config` 就是同线程死锁,而命令跑在 UI 线程上 → 表现为"软件无响应"。
  改内存用 `update_widget` / `update_firmware`,它们把锁收紧在改内存那一小段里。
- **设备状态只有一份订阅**:`app/src/store.ts`(`ensureListening()`)。标题栏的状态段不在
  任何一页里,不能让某一页的 `onMounted` 快照说了算;页面被 `<KeepAlive>` 保活后切回来
  不会重跑 `onMounted`,会一直显示过期状态。
- **全局 `input` 规则只给文本类控件**:`input:not([type="checkbox"]):not([type="radio"])`。
  分段控件与开关里的 checkbox/radio 是**透明覆盖**在自定义外观上的,被套上
  `height/padding/背景` 会变成看不见但更大的热区。
- **按钮居中靠 `inline-flex`,不靠 `height`**:`.btn` 的高度只有两种来源 ——
  `height: var(--control-h)` 或 `padding` 撑高,**两者不混用**(混用会把文字挤偏)。
- 前端调 `getCurrentWindow()` 之类**要在函数里取**,不要放模块顶层:非 Tauri 环境
  (浏览器里看构建产物)会抛异常,整个组件挂掉、界面缺一条。

## 设备名与链路

设备按名称 `AI Passport Vibe Mic` 定位,**不接受 MAC**。名字匹配忽略大小写与空白,
并保留别名 `Passport VibeMic` —— 系统会缓存已配对设备的名字,改名不会让桌面端连不上。
`--timeout` 只对 btleplug 的 BLE 扫描路径生效。

## 图标

源文件 `app/src-tauri/icons/icon.svg`(圆角方块紫渐变底 + 白色麦克风)。改动后重新生成全套:

```bash
cd app
npx tauri icon <1024px-PNG> -o src-tauri/icons   # exe/托盘/窗口全套
```

SVG → PNG 用 `@resvg/resvg-js` 渲染 1024×1024(圆角外保持透明)。

## 行为契约

- **托盘常驻**:左键单击/双击打开配置页面,右键菜单 = 显示主面板 / 切换悬浮窗 / 退出;
  主面板点关闭 = 隐藏到托盘。菜单走右键:左键先弹菜单会把第二次点击吞在菜单上,
  双击就到不了托盘事件。
- **单实例**:重复启动不起第二份(`tauri-plugin-single-instance`)。必须有:否则多出第二个
  托盘图标 + 第二个悬浮窗,两个进程还会抢同一个 BLE 连接。第二个进程**把已在跑的面板
  唤到前台后自退**(`show_panel`,与托盘左键、悬浮窗双击同一路径)—— 用户手动双击的意图
  就是要用它,什么都不做会表现为"点了没反应"。实测:第二个进程退出码 0,日志只多一行
  「检测到已运行的实例…」,第一个进程照常运行。
- **排障开关**:`VibeMic.exe --show-panel` 启动即打开配置页面(默认不弹,便于改 UI 时截图核对);
  日志在 `%APPDATA%\vibemic\app.log`(满 1MB 轮转到 `app.log.1`)。
- **悬浮窗**:30px 高深色胶囊,宽度随内容自适应(81~117,不是固定卡片);**状态只由左侧
  元素表达** —— 未连接(⊘)/待机(麦克风)/录音中(真实电平波形 + 计时)/识别中·下载中(旋转弧)/
  链路不稳(三角警告),文字里不带 `●`、`⚠` 这类符号,避免与左侧状态图形重复表达同一件事。
  透明置顶无框不抢焦点;**不显示识别文本**(只做状态指示,识别结果只键入焦点窗口);
  拖动移动,双击打开配置页面,右键菜单(固定位置 / 关闭)。
  **胶囊的立体感只能用内阴影**:窗口高度与胶囊相同(30px),外阴影的模糊半径整个
  落在窗口外、会被窗口矩形裁成一条**平的深色带**,看着就是"胶囊外边还有个长方形
  背景色";中性态是 1px 白色内描边 + 1px 顶部高光,录音/告警态只换环色,
  `.s-rec`/`.s-warn` 不要再加外阴影。
- **主面板(左侧导航 5 页)**:**没有独立的「状态」页** —— 设备连接状态、电量、固件版本
  在标题栏常显(软件名之后,隔一道竖线,未连接时电量与固件整块收起)。
  五页 = **语音识别**(模型状态/位置/自检)、**按键**(映射表 + 确认键设置)、
  **固件烧录**(见下)、**设置**(开机自启动 / 主题三态 / 本机信息)、
  **关于**(图标 + 版本号 + 检查更新)。
  视觉上的取舍(间距、行高、为什么这样排)都写在 `app/src/style.css` 与 `app/src/pages/*.vue`
  的注释里 —— **没有独立的设计稿文件**:`style.css` 是唯一的样式来源,改 UI 只改它。
- **开机自启动**:设置页一个开关,走 `tauri-plugin-autostart`(Windows 即写
  `HKCU\...\Run`)。**选中态以 `isEnabled()` 回读为准**,不在 `config.json` 存副本 ——
  真实来源是系统启动项;写入失败就地给提示 + 「重试」。
- **检查更新**(关于页):问 Gitee(主源)与 GitHub(备源)的 `releases/latest`,
  两路并行、各 15s 超时、谁成功用谁、两边都成功以 Gitee 为准、单边失败静默;
  版本号**逐段数值比较**(字符串序会把 1.1.10 判成小于 1.1.9),预发布后缀解析时截掉。
  请求走**后端**(`app/src-tauri/src/update.rs`):主窗口 CSP 不允许页面直连外网,
  且"上次应答的发布页"记在后端,`open_release_page` 只认它(IPC 面不含任意 URL)。
  本地版本取 `package_info`(= `tauri.conf.json` 的 `version`,与安装包同一口径)。
- **固件烧录(应用内,espflash)**:
  - 页头显示**当前固件版本**(未连接时写「未知」);产物与偏移严格照官方交付方式:
    **只烧合并镜像 `FoloToy-AI-Passport-full.bin`,固定写 `0x0`**;
    写前验证 C3 bootloader/应用段、校验和与内嵌 SHA-256,验证 `0x8000` 分区表 MD5、边界和重叠;
    当前支持含 factory 应用的标准合并布局,另显示全文件 SHA-256 供比对,
    任一条不过就拒绝烧录。写入用 `espflash`(esp-rs 的 esptool 移植,自述 "based on esptool.py"),
    与官方 `python -m esptool … write-flash 0x0` 产物同源、偏移相同 —— 代价是它不是同一个程序,
    需要严格一致时可另加"调用系统 esptool"的备用路径。
  - 镜像默认就**在软件所在目录里找**:先看 `firmware/FoloToy-AI-Passport-full.bin`
    (交付包的摆法),再看 exe 同目录的同名文件(散装摆法),先命中的那条生效
    (`config.json` 的 `firmware.path` 留空即此约定);**手选过路径则严格只用它** ——
    哪怕文件已被删掉也照报错,绝不悄悄换成别的镜像去烧。面板上用「**选择 .bin…**」调系统
    文件对话框手动挑,只列 `.bin`;对话框的默认值 `defaultPath` 取后端算好的
    `flash_info.default_path`,**它只认真实存在的文件**:软件目录里翻不到镜像时它是 `null`,
    前端于是不传 `defaultPath`,让用户自己挑目录 —— 拿不存在的路径当默认值会被 Windows
    对话框把文件名预填进"文件名"框,点确定只得到"找不到文件"。镜像那一行只有「选择 .bin…」。
    波特率(默认 460800)与写后回读校验同样在面板里改,一起存进 `%APPDATA%/vibemic/config.json`。
  - 端口由 `serialport` 枚举;命中 Espressif USB VID `0x303A` + USB-Serial/JTAG PID `0x1001`
    的端口会标 `USB-JTAG` —— 这类设备自带下载模式,不用按 BOOT。**传给 espflash 的必须是带真实
    pid 的端口信息**:它据此选择 USB-JTAG 复位序列,填 0 会直接连不上。
  - 烧录跑在独立线程(几十秒,不能占 WebView2 的 UI 线程),进度/日志/结果经
    `flash-progress` / `flash-log` / `flash-done` 回吐;`FLASHING` 标志保证同一时刻只有一次,
    免得两个线程抢同一个串口。
  - ⚠️ 合并镜像从 `0x0` 写入会覆盖 NVS:设备端激活档回默认(Trae)、**蓝牙要重新配对一次**。
    这一条用**红色警示条**(`.notice-danger`:浅红底 + 红字 + 红描边)写在写入按钮下方,
    右侧直接给「打开蓝牙设置」——它要在**按下写入之前**被看到。
- **烧录后的配对善后(刻意做到最简)**:合并镜像清掉设备侧 NVS 后,Windows 这边**还留着旧密钥**,
  之后每次连接都会在加密协商上失败,表现就是"蓝牙突然连不上了"。处理方式只有一件事:
  红色警示条上的「**打开蓝牙设置**」(`explorer.exe ms-settings:bluetooth`)
  —— **没有说明文字、没有检测、没有代删条目**。
  **这一页不要再往回加自动解绑**:同一块板子可以刷不同软件,系统里留下的旧条目可能挂着
  完全无关的名字,枚举配对表、按关键词猜"这是不是旧名字"、自动 `unpair()` 都可能误伤
  用户的其它设备;而刷过别的固件留下的怪名字,用户在系统设置页里看得见、也删得掉 ——
  把用户送到那儿就够了。`uuid::device_name_matches` 只用于连接时认设备,不承担这个用途。
- **深浅主题**:三态(跟随系统/深/浅),偏好存 localStorage 跨窗口即时同步;
  首帧由各 html 内联脚本定色避免闪色。
- **按键分发**:设备 BUTTON_EVENT → UP=确认(Enter/Ctrl+Enter,固件 ≥0x0002)、DOWN=Esc、
  DOWN 双击=停止(连发两记 Esc,间隔 60ms,给 DSH 这类「双击 Esc 中断」的软件)、
  OK 双击=删除(第二下按下即删一个字符,继续按住则每 100ms 再删一个直到松手);
  OK 按住 = 录音 → 本地识别 → 键入焦点;UP/DOWN 等通用键在 PC 离线时仍由设备直发 HID。
  删除手势只在桌面端在线时可用(裸键盘模式下没有它),原因见下条。
  **DOWN 长按 10 秒 = 设备端蓝牙硬重置**(清空全部 bond + 断开 + 重新广播):全在设备侧
  执行,桌面端既不发起也不参与,设备离线/链路卡死时照样可用;代价是主机还留着旧密钥,
  用户必须在系统蓝牙设置里删除本设备再重新配对(固件烧录页的「打开蓝牙设置」同理)。
- **OK 双击为什么由桌面端判定**:按键组件要等第二下**抬起**、再过短按窗口才补发 DOUBLE,
  按住期间一个事件都不发,"按住连删"无从谈起;`ButtonEvent` 自带设备侧 `ts_ms`,
  所以桌面端按"上次抬起 → 这次按下是否 ≤180ms"自己判,并用同一标志跳过这一下的
  录音管线。设备侧(固件 ≥0x0005)用同一个 180ms 窗口独立判一次:第二下不启动 PTT、
  麦克风不开、屏上显示 `DELETE`;更早的固件仍会推流,那些帧到 PC 后直接丢弃,不会打字。
- **Windows 链路**:`vibemic-link` 的 WinRT 直连同进程使用(按名枚举已配对设备 +
  FromIdAsync,缓存优先服务发现 + CCCD 双写去缓存失同步)。
  固件 ≥0x0006 使用加密的 `CMD_PC_SESSION(0x05)` 应用租约:payload `1` 续租15秒,`0` 结束。
  WinRT/btleplug 在全部订阅成功后激活,并在 poll 中每5秒续租(音频期间也续租)。
  CCCD 本身不代表应用在线;退出/回滚将远端 CCCD 写回 None,失败时由租约失效兜底。
  升级先装新版桌面端再刷新版固件;旧桌面端与新固件仅支持裸 HID。
  设备线程带 45s 心跳看门狗,WinRT 真挂住时 UI 会标记未连接而不是假装还连着。
  **链路活性以"设备有没有回帧"为准**:静默时每 5s 发一次 `GET_STATUS` 探测,
  连续 2 次探测没有任何上行帧即判失效并断链重连(设备关机 ≈5s 内、通知被静默丢弃的
  半死会话 ≈15s 内就会被发现);连接失败重试退避封顶 60s,设备上电后约 1 分钟内自愈。
  链路层诊断信息(服务发现重试、哪个特征值订阅失败)经 `vibemic_link::set_logger` 写进 `app.log`。

## 语音输入前的标点衔接

`text_context.rs` 通过 Windows UI Automation 读取实际光标前最多 16 个字符。
仅在焦点稳定、可编辑、非密码、单个折叠选区时判断是否补 `。`；已有标点或换行不补。
范围必须属于当前输入控件，并在其 DocumentRange 边界内；零宽占位字符不算已有文字。
`GetEnclosingElement` 可返回输入框内的段落等后代节点；必须沿父链确认归属，不能要求与焦点元素完全相等，也不能接受输入框外的页面祖先或兄弟节点。
Chromium 的末尾光标可能排序在文本节点终点之后；仅当光标仍属于同一输入框、间隔范围不含任何字符时，允许将读取端点收敛到文档末尾。保留原始光标用于最后的选区一致性检查。
不得用整个输入框末尾或上次转写替代光标位置。读取在专用 MTA 线程运行，最多等待 200ms，
同时至多一个查询；不支持、失败、超时或焦点改变都静默回退原文，不弹错误、不记录输入框内容。

## 尚未实现(别在文档/界面里写成已有)

双主机(bond A/B)、闲置功耗、安装包与签名、macOS 设备链路、设备端串口帧服务、
「识别后自动发送」(界面不提供,理由见上「主面板」那条)。
