//! VibeMic 桌面应用(简化的控制模型):
//!
//! - 设备链路线程:WinRtLink 同进程常驻重连;OK 按住收音→松开本地识别→键入焦点;
//!   UP=确认(Enter/Ctrl+Enter 按设置)、DOWN=取消(固件 ≥0x0002 起 PC 分发)、
//!   DOWN 双击=停止(两记 Esc)、OK 双击=删除(按住连删)
//! - 托盘:显示/隐藏面板与悬浮窗、退出
//! - 主面板:左侧导航 5 页(语音识别/按键/固件烧录/设置/关于),
//!   设备连接状态与电量在标题栏常显;开机自启动走 tauri-plugin-autostart,
//!   检查更新走 [`update`]
//! - 配置:%APPDATA%/vibemic/config.json(确认键 + 悬浮窗几何 + 固件烧录)

mod dispatch;
mod text_context;
mod diag;
mod flash;
mod firmware_image;
mod state;
mod update;
#[cfg(target_os = "windows")]
mod device;

use std::sync::Arc;

use state::{AppState, SendKey, Shared};
use tauri::{Emitter, Manager, WebviewWindowBuilder, WindowEvent};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

const WIDGET: &str = "widget";
const MAIN: &str = "main";

#[tauri::command]
fn get_state(shared: tauri::State<'_, Arc<Shared>>) -> AppState {
    shared.state_guard().clone()
}

/// 确认键设置(主面板单选组)。
///
/// 写成 async + spawn_blocking 是有原因的:非 async 的 `#[tauri::command]` 由
/// tauri-macros 生成 `body_blocking`,**在 IPC 回调里原地执行**——Windows 上
/// 这个回调就是 WebView2 的 UI 线程(= Tauri 主线程)。落盘是阻塞文件写,放主
/// 线程上写会卡消息泵,所以凡是碰磁盘的命令一律挪到 blocking 线程池。
#[tauri::command]
async fn set_send_key(
    shared: tauri::State<'_, Arc<Shared>>,
    send_key: SendKey,
) -> Result<(), String> {
    let shared = shared.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        {
            let mut s = shared.state_guard();
            s.send_key = send_key;
        }
        shared.save_config()
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 悬浮窗拖动停稳后落盘(逻辑像素;前端负责除以缩放因子换算)。
///
/// 必须走 `Shared::update_widget`:它只在改内存时持 widget 锁,
/// 落盘在锁外。持锁调 `save_config` 就是自锁,例如
/// ```ignore
/// let mut w = shared.widget.lock().unwrap();   // 持锁
/// w.x = x; w.y = y;
/// shared.save_config();                        // 内部再 lock 一次 widget → 自锁
/// ```
/// 死锁发生在 UI 主线程上,所以症状是「拖一下悬浮窗(或只是单击一下,startDragging
/// 的 promise 落定后 220ms 也会走到这里)之后,整个软件无响应」。
#[tauri::command]
async fn save_widget_position(
    shared: tauri::State<'_, Arc<Shared>>,
    x: f64,
    y: f64,
) -> Result<(), String> {
    let shared = shared.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        shared.update_widget(|w| {
            w.x = x;
            w.y = y;
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 右键菜单「固定位置」:锁定当前位置,不再响应拖动。
/// (锁的约束与 [`save_widget_position`] 相同:持 widget 锁调 save_config 即自锁。)
#[tauri::command]
async fn set_widget_pinned(
    shared: tauri::State<'_, Arc<Shared>>,
    pinned: bool,
) -> Result<(), String> {
    let shared = shared.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        shared.update_widget(|w| w.pinned = pinned)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 烧录进行中标志:同一时刻只允许一次,免得两个线程抢同一个串口。
static FLASHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 打开系统的蓝牙设置页(烧录后重新配对用)。
///
/// 配对善后就到这一步为止 —— 不枚举系统配对表、不代删条目、不判断配没配上:
/// 刷过别的固件时旧条目可能挂在完全不同的名字下,自动判断难免误伤别人的设备,
/// 让用户在系统页里自己看、自己删最稳妥。面板上只多一块"蓝牙需要重新配对 + 这个按钮"。
#[tauri::command]
fn open_bluetooth_settings() {
    // 交给 explorer 解析 ms-settings: 协议,比 cmd /c start 少闪一个控制台窗口。
    if let Err(e) = std::process::Command::new("explorer.exe")
        .arg("ms-settings:bluetooth")
        .spawn()
    {
        diag::log(&format!("打开蓝牙设置失败: {e}"));
    }
}

/// 本机信息:面板上要展示、要"打开"的几个路径。
///
/// 路径由后端给,前端不拼 —— 这样"界面上显示的路径"与"实际读写的路径"永远是同一个
/// 口径(改目录时只改一处)。只 expose 应用自己的目录,不是通用的"打开任意路径"入口。
#[derive(serde::Serialize)]
struct AppPaths {
    /// %APPDATA%\vibemic
    config_dir: String,
    /// config.json 全路径
    config_file: String,
    /// 语音模型目录
    model_dir: String,
    /// 日志目录(实际写的是 app.log,这里给目录方便"打开所在位置")
    log_dir: String,
}

fn app_root() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("vibemic")
}

fn app_paths_inner() -> AppPaths {
    let root = app_root();
    AppPaths {
        config_dir: root.display().to_string(),
        config_file: root.join("config.json").display().to_string(),
        model_dir: vibemic_asr::model_dir().display().to_string(),
        log_dir: root.display().to_string(),
    }
}

#[tauri::command]
fn app_paths() -> AppPaths {
    app_paths_inner()
}

/// 在资源管理器里打开应用自己的某个目录(设置页「打开配置目录 / 打开日志目录」)。
///
/// `which` 只认白名单里的几个值,不接受任意路径:命令面越小越好,
/// 也免得把"用 shell 打开用户给的路径"变成一条通用通道。
#[tauri::command]
fn open_app_dir(which: String) -> Result<(), String> {
    let target = match which.as_str() {
        "config" => app_root().join("config.json"),
        "log" => app_root().join("app.log"),
        "models" => vibemic_asr::model_dir(),
        other => return Err(format!("未知的目录标识: {other}")),
    };
    // 文件不存在时 explorer 会静默失败(还可能弹出"找不到"对话框),先确认一遍:
    // 目录不存在就退到它的父目录,都拿不到才报错。
    let open_target = if target.exists() {
        target
    } else if let Some(parent) = target.parent() {
        if parent.exists() {
            parent.to_path_buf()
        } else {
            return Err(format!("目录不存在: {}", parent.display()));
        }
    } else {
        return Err(format!("路径不存在: {}", target.display()));
    };
    // explorer 打开目录:成功也会返回非 0 退出码,所以不看状态,只看能不能起来
    open_in_file_manager(&open_target)
        .map(|_| ())
        .map_err(|e| format!("打开目录失败: {e}"))
}

/// 在系统文件管理器里打开一个路径(目录或文件所在位置)。
///
/// 设备链路目前是 Windows-only,但面板不该跟着变成"只在 Windows 上能编译":
/// 非 Windows 用系统惯例命令(macOS `open` / 其它 `xdg-open`)。
fn open_in_file_manager(path: &std::path::Path) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer.exe").arg(path).spawn()
    }
    #[cfg(not(windows))]
    {
        let bin = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(bin).arg(path).spawn()
    }
}

/// 枚举串口(只读:只列名字与 USB 信息,不打开任何端口)。
#[tauri::command]
fn flash_ports() -> Vec<flash::PortEntry> {
    flash::list_ports()
}

/// 待烧镜像的自检:解析路径 → 读文件算 SHA-256 → 查 ESP 镜像魔数。
///
/// 要读整个镜像(几 MB),按本文件的规矩走 blocking 线程池:非 async 的
/// `#[tauri::command]` 由 tauri-macros 在 IPC 回调里原地执行,而 Windows 上那就是
/// WebView2 的 UI 线程,阻塞文件读写会卡住整个消息泵。
#[tauri::command]
async fn flash_info(shared: tauri::State<'_, Arc<Shared>>) -> Result<flash::FirmwareInfo, String> {
    let configured = shared.state_guard().firmware.path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        flash::inspect(&flash::resolve_firmware_path(&configured))
    })
    .await
    .map_err(|e| e.to_string())
}

/// 改固件烧录设置。`path` 传空串 = 回到"在软件所在目录里找镜像"的默认约定
/// (先 `firmware/` 子目录,再 exe 同目录)。
#[tauri::command]
async fn set_firmware_config(
    app: tauri::AppHandle,
    shared: tauri::State<'_, Arc<Shared>>,
    path: Option<String>,
    baud: Option<u32>,
    verify: Option<bool>,
) -> Result<(), String> {
    let shared = shared.inner().clone();
    let for_task = shared.clone();
    tauri::async_runtime::spawn_blocking(move || {
        for_task.update_firmware(|f| {
            if let Some(path) = path {
                f.path = path;
            }
            if let Some(baud) = baud {
                f.baud = baud;
            }
            if let Some(verify) = verify {
                f.verify = verify;
            }
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    broadcast(&app, &shared);
    Ok(())
}

/// 开始烧录:合并镜像从 0x0 写入。
///
/// 立刻返回,真正的写入跑在独立线程上(几十秒,绝不能占 UI 线程),进度与日志经
/// `flash-progress` / `flash-log` / `flash-done` 三个事件回吐。
#[tauri::command]
async fn flash_start(
    app: tauri::AppHandle,
    shared: tauri::State<'_, Arc<Shared>>,
    port: String,
    erase: bool,
) -> Result<(), String> {
    let shared = shared.inner().clone();
    let (configured, baud, verify) = {
        let s = shared.state_guard();
        (s.firmware.path.clone(), s.firmware.baud, s.firmware.verify)
    };
    let path = flash::resolve_firmware_path(&configured);

    if FLASHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("已经有一次烧录在进行中".to_string());
    }

    // 烧录线程结束后要写状态并广播,所以把 Shared 也带进去(它本身就是 Arc)。
    let shared_in_thread = shared.clone();
    let spawned = std::thread::Builder::new()
        .name("vibemic-flash".to_string())
        .spawn(move || {
            use tauri::Emitter;
            let mut sink = |event: flash::FlashEvent| match event {
                flash::FlashEvent::Progress { done, total } => {
                    let _ = app.emit(
                        "flash-progress",
                        serde_json::json!({ "done": done, "total": total }),
                    );
                }
                flash::FlashEvent::Verifying => {
                    let _ = app.emit("flash-log", "回读校验中…");
                }
                flash::FlashEvent::Written { skipped } => {
                    let _ = app.emit(
                        "flash-log",
                        if skipped {
                            "内容与设备上一致,已跳过写入"
                        } else {
                            "写入完成"
                        },
                    );
                }
                flash::FlashEvent::Log(text) => {
                    let _ = app.emit("flash-log", text);
                }
            };

            let request = flash::FlashRequest {
                port: &port,
                path: &path,
                baud,
                verify,
                erase,
                chip: flash::chip_from_name("esp32c3").unwrap_or(espflash::target::Chip::Esp32c3),
            };
            let result = flash::flash(&request, &mut sink);
            match &result {
                Ok(()) => {
                    diag::log(&format!("烧录完成:{} → {}", path.display(), port));
                    // 合并镜像从 0x0 写入会清掉 NVS:设备侧绑定没了,系统这边旧条目还在。
                    // 只在面板上多出一块"蓝牙需要重新配对 + 打开蓝牙设置",别的什么都不做。
                    {
                        let mut s = shared_in_thread.state_guard();
                        s.pairing_needed = true;
                    }
                    broadcast(&app, &shared_in_thread);
                }
                Err(e) => diag::log(&format!("烧录失败: {e}")),
            }
            let _ = app.emit(
                "flash-done",
                serde_json::json!({
                    "ok": result.is_ok(),
                    "error": result.err(),
                }),
            );
            FLASHING.store(false, std::sync::atomic::Ordering::SeqCst);
        });

    if let Err(e) = spawned {
        FLASHING.store(false, std::sync::atomic::Ordering::SeqCst);
        return Err(format!("烧录线程创建失败: {e}"));
    }
    Ok(())
}

/// 悬浮窗右键菜单「关闭悬浮窗」= 隐藏(托盘「切换悬浮窗」可再唤出)。
#[tauri::command]
fn close_widget(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(WIDGET) {
        let _ = w.hide();
    }
}

/// 双击悬浮窗打开主面板(与托盘「显示主面板」同一路径):隐藏状态打开时先居中,
/// show + unminimize + set_focus。
#[tauri::command]
fn show_panel(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        if matches!(w.is_visible(), Ok(false)) {
            let _ = w.center();
        }
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 下载语音模型(阻塞线程内执行;进度经 state 广播,前端实时可见)。
#[tauri::command]
fn download_model(app: tauri::AppHandle, shared: tauri::State<'_, Arc<Shared>>) {
    start_model_download(app, shared.inner().clone(), false);
}

/// 重新下载:先删掉现有模型文件再下。用于"文件在、引擎却加载失败"的死胡同
/// ——那种情况下 `download_model` 会直接判定就绪并立刻返回 Ok,点了没反应。
#[tauri::command]
fn redownload_model(app: tauri::AppHandle, shared: tauri::State<'_, Arc<Shared>>) {
    start_model_download(app, shared.inner().clone(), true);
}

fn start_model_download(app: tauri::AppHandle, shared: Arc<Shared>, reset: bool) {
    let Some(model_task) = shared.try_model_task() else {
        diag::log("模型正用于下载或识别,本次下载请求未启动");
        return;
    };
    // 线程闭包要拿走这两个,失败分支还要用来落错误状态,所以先克隆。
    let thread_app = app.clone();
    let thread_shared = shared.clone();
    let spawned = std::thread::Builder::new()
        .name("vm-model-dl".into())
        .spawn(move || {
            let _model_task = model_task;
            {
                let mut s = thread_shared.state_guard();
                s.asr = state::asr_state::DOWNLOADING.into();
                s.download_pct = 0;
            }
            broadcast(&thread_app, &thread_shared);
            if reset {
                match vibemic_asr::reset_model() {
                    Ok(()) => diag::log("已清除本地语音模型,开始重新下载"),
                    Err(e) => diag::log(&format!("清除本地语音模型失败: {e}")),
                }
            }
            let shared2 = thread_shared.clone();
            let result = vibemic_asr::download_model(&mut |done, total| {
                let pct = (done * 100 / total.max(1)).min(100) as u8;
                if let Ok(mut s) = shared2.state.try_lock() {
                    s.download_pct = pct;
                }
                broadcast(&thread_app, &shared2);
            });
            // state 锁必须先放掉再 broadcast:broadcast 内部还要取一次 state,
            // 持锁调用即同线程重入自锁——下载线程会永久挂在这儿、还一直占着 state
            // 锁,之后主线程的 get_state/broadcast 全部跟着卡死(整机无响应)。
            // 别把 broadcast 挪回上面那个块里,也别删掉这层作用域。
            {
                let mut s = thread_shared.state_guard();
                match result {
                    Ok(()) => {
                        s.asr = state::asr_state::READY.into();
                    }
                    Err(ref e) => {
                        s.asr = format!("error: {e}");
                    }
                }
            }
            match result {
                Ok(()) => diag::log("语音模型下载完成"),
                Err(e) => diag::log(&format!("语音模型下载失败: {e}")),
            }
            broadcast(&thread_app, &thread_shared);
        });
    if let Err(e) = spawned {
        diag::log(&format!("模型下载线程创建失败: {e}"));
        shared.state_guard().asr = format!("error: 下载线程创建失败: {e}");
        broadcast(&app, &shared);
    }
}

fn broadcast(app: &tauri::AppHandle, shared: &Shared) {
    let snapshot = shared.state_guard().clone();
    let _ = app.emit("state", snapshot);
}

/// 双击第二次时把已在跑的那个面板唤到前台,然后让第二个实例自己退出。
///
/// 为什么不是"什么都不做":用户是**手动双击 exe** 才起的第二个进程,
/// 那时他的意图就是"我要用这个软件";什么都不做会表现为"点了没反应"
/// (软件在跑,但常驻托盘,面板没出来)。这里直接调 `show_panel`,
/// 与托盘左键单击、悬浮窗双击走同一条路径。
///
/// `--show-panel` 也照办:调它的是排障/截图流程(`VibeMic.exe --show-panel`),
/// 那次点击同样期望把面板调出来。
fn on_second_instance(app: &tauri::AppHandle, args: Vec<String>) {
    // 后面还有没有人真的第二次启动这个 exe?留在日志里,排查"为什么点不开"时有用
    diag::log(&format!(
        "检测到已运行的实例,本次启动不再建窗口(参数 {} 个),改为唤出已有面板",
        args.len().saturating_sub(1)
    ));
    show_panel(app.clone());
    // 面板可能还停在上次的位置/被最小化:show_panel 里统一做了 center + show + unminimize + focus
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let shared = Arc::new(Shared::new());
    shared.load_config();
    diag::init();
    // 链路层的诊断信息(服务发现重试、哪个特征值订阅失败)接到 app.log:托盘程序
    // 没有可见 stderr,不接的话这些信息在 GUI 进程里直接消失。
    vibemic_link::set_logger(diag::log);
    shared.state_guard().asr = if vibemic_asr::model_ready() {
        state::asr_state::READY.into()
    } else {
        state::asr_state::MISSING.into()
    };

    let app_shared = shared.clone();
    tauri::Builder::default()
        // 单实例闸门,必须在最前面:插件用一个本地 socket 抢名字,抢不到就说明
        // 已有实例在跑 —— 于是回调上面的 on_second_instance 并**终止本进程**,
        // 下面所有插件、托盘、悬浮窗、设备线程都不会再建。
        //
        // 顺序上还有个坑:它要排在 dialog / autostart 之前,否则第二个实例会先把
        // 这些插件初始化一遍(注册表读取、对话框子系统)再退出,白做一轮初始化。
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            on_second_instance(app, args)
        }))
        // 选固件用的系统打开对话框(内部就是 rfd);权限见 capabilities/default.json。
        .plugin(tauri_plugin_dialog::init())
        // 开机自启动:写系统的启动项(HKCU\...\Run)。只有前端在调,
        // Rust 侧不需要自己的命令;权限见 capabilities/default.json 的 autostart:allow-*。
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_send_key,
            save_widget_position,
            set_widget_pinned,
            close_widget,
            show_panel,
            download_model,
            redownload_model,
            flash_ports,
            flash_info,
            set_firmware_config,
            flash_start,
            open_bluetooth_settings,
            app_paths,
            open_app_dir,
            update::check_update,
            update::open_release_page
        ])
        .setup(move |app| {
            // ---- 托盘 ----
            let show = MenuItem::with_id(app, "show", "显示主面板", true, None::<&str>)?;
            let widget = MenuItem::with_id(app, "widget", "切换悬浮窗", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &widget, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("VibeMic（双击打开配置页面）")
                .menu(&menu)
                // 菜单改由**右键**唤出:左键留给"打开主面板"(与悬浮窗双击同一路径)。
                // 若保留左键弹菜单,第一次点击会先把菜单弹出来,双击就落在菜单上了。
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window(MAIN) {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "widget" => {
                        if let Some(w) = app.get_webview_window(WIDGET) {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                            }
                        } else {
                            let _ = build_widget(app);
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                // 双击(以及单击)托盘图标 = 打开配置页面,与双击悬浮窗同一路径。
                // 单击也给上:菜单已移到右键,否则单击会"什么都没发生"。
                .on_tray_icon_event(|tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => show_panel(tray.app_handle().clone()),
                    _ => {}
                })
                .build(app)?;

            // ---- 悬浮窗(常显) ----
            build_widget(app.handle())?;
            // 主面板首次启动不弹,托盘唤出(双击 exe 后台就绪)。
            // 例外:`VibeMic.exe --show-panel` 直接打开面板 —— 排障与改 UI 时用得上
            // (否则每次都得先去点托盘,没法用截图核对样式)。延后 1s 再开:
            // setup 阶段 config 里的窗口可能还没建好,那时 get_webview_window 是 None。
            if std::env::args().any(|a| a == "--show-panel") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    show_panel(handle);
                });
            }

            // ---- 后台线程 ----
            // 链路同进程:WinRT 跑在 device-link 线程里,交付物只有一个 exe。
            // 万一 WinRT 真的挂住,device.rs 的心跳看门狗会把 UI 标成未连接,
            // 而不是一直骗人说已连接。
            #[cfg(target_os = "windows")]
            device::spawn(app.handle().clone(), app_shared.clone());
            #[cfg(not(target_os = "windows"))]
            let _ = &app_shared;
            Ok(())
        })
        .on_window_event(|window, event| {
            // 主面板点关闭 = 隐藏到托盘(应用常驻)。
            if window.label() == MAIN {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn build_widget(app: &tauri::AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(WIDGET).is_some() {
        return Ok(());
    }
    /// 悬浮窗首帧尺寸(逻辑像素),与 Widget.vue 的 INIT_W/CARD_H 一致:
    /// 待机态胶囊 93×30。真实宽度由前端量内容后 setSize 收窄/放宽。
    const INIT_W: f64 = 93.0;
    const INIT_H: f64 = 30.0;
    let shared = app.state::<Arc<Shared>>();
    let w = *shared.widget_guard();
    // 初始 pinned 状态经初始化脚本注入(页面脚本跑之前);位置/尺寸口径与
    // Widget.vue 的 CARD_W/CARD_H 常量保持一致(逻辑像素)。
    let init = format!("window.__WIDGET_INIT__ = {{ pinned: {} }};", w.pinned);
    let win = WebviewWindowBuilder::new(
        app,
        WIDGET,
        tauri::WebviewUrl::App("widget.html".into()),
    )
    .title("VibeMic")
    .transparent(true)
    .decorations(false)
    // shadow 必须显式关掉(默认 true):透明无框窗口若带 DWM 阴影,整个窗口
    // 矩形会挂一圈可见边框——右键菜单撑高窗口时,看起来就是悬浮窗变出大框
    .shadow(false)
    .resizable(false)
    .minimizable(false)
    .maximizable(false)
    .closable(false)
    .skip_taskbar(true)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    // focusable(false):悬浮窗常显在别人窗口之上,点它不抢焦点,拖动交给前端的
    // startDragging(代价是窗口收不到焦点事件,右键菜单只能靠光标离开窗口收)。
    .focusable(false)
    // 悬浮窗上拖文件不该弹 WebView2 的拖放覆盖层
    .disable_drag_drop_handler()
    // 初始宽度取"待机态"的 93(见 Widget.vue 的 CARD_H/INIT_W):胶囊宽度由内容
    // 决定,前端首帧量到真实宽度后会 setSize 收窄/放宽;这里只保证第一帧不裁字。
    //
    // min_inner_size 必须显式给:Windows 对顶层窗口有 SM_CXMINTRACK/SM_CYMINTRACK
    // 的默认下限(125% 缩放下约 170×47 物理像素 ≈ 136×39 逻辑),
    // 不覆盖的话 30px 高的胶囊窗口根本压不下去 —— 表现为胶囊左边一条、右边一大片
    // 透明死区。tao 只在 size_constraints 有值时改写 WM_GETMINMAXINFO,给了就生效。
    .min_inner_size(60.0, 26.0)
    .inner_size(INIT_W, INIT_H)
    .position(w.x, w.y)
    .initialization_script(&init)
    .build()?;
    // 建窗时 CreateWindowEx 会先把尺寸夹到系统下限(WM_GETMINMAXINFO 早于
    // min_inner_size 写入约束),所以 init_w/init_h 得再设一次才是真的 ——
    // 前端只在"量到的宽度 != INIT_W"时才 setSize,待机态正好相等会跳过,不会自愈。
    let _ = win.set_size(tauri::LogicalSize::new(INIT_W, INIT_H));
    Ok(())
}
