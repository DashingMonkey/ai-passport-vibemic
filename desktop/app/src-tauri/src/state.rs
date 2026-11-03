//! 应用共享状态与配置持久化。
//!
//! 控制模型:OK=语音转文字直键入焦点,UP=确认,DOWN=取消。可配置项只有"确认键"
//! (Enter/Ctrl+Enter)与悬浮窗几何。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// 确认键(UP 单击注入):绝大多数软件发送是 Enter,少数 IDE 对话框为 Ctrl+Enter。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SendKey {
    #[default]
    Enter,
    #[serde(rename = "ctrl_enter")]
    CtrlEnter,
}

/// 悬浮窗几何与固定状态:位置为逻辑像素,拖动停稳后由前端落盘,
/// 重启/重建窗口时原位恢复。pinned = 锁定当前位置不再响应拖动。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WidgetCfg {
    #[serde(default = "default_widget_x")]
    pub x: f64,
    #[serde(default = "default_widget_y")]
    pub y: f64,
    #[serde(default)]
    pub pinned: bool,
}

fn default_widget_x() -> f64 {
    1200.0
}
fn default_widget_y() -> f64 {
    120.0
}

impl Default for WidgetCfg {
    fn default() -> Self {
        WidgetCfg {
            x: default_widget_x(),
            y: default_widget_y(),
            pinned: false,
        }
    }
}

/// 固件烧录配置。留空 path = 在软件所在目录里找合并镜像:先看 `firmware/` 子目录,
/// 再看 exe 同目录(两种摆法都认,"PC 软件和设备固件放同一个文件夹"的约定)。
/// 手选过 path 则严格只用它 —— 文件没了就报错,绝不悄悄换成别的镜像。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareCfg {
    #[serde(default)]
    pub path: String,
    /// 烧录波特率(连接阶段固定 115200,连上后再切到这个值)。
    #[serde(default = "default_flash_baud")]
    pub baud: u32,
    /// 写后回读校验:慢一点,但能挡住"写了一半"。
    #[serde(default = "default_true")]
    pub verify: bool,
}

fn default_flash_baud() -> u32 {
    crate::flash::DEFAULT_BAUD
}

fn default_true() -> bool {
    true
}

impl Default for FirmwareCfg {
    fn default() -> Self {
        FirmwareCfg {
            path: String::new(),
            baud: default_flash_baud(),
            verify: true,
        }
    }
}

/// config.json 整体结构:确认键 + 悬浮窗几何 + 固件烧录。反序列化时缺字段走各自的默认值,
/// 因此读到只含部分字段(例如只有 widget)的历史文件也不会失败。
#[derive(Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    send_key: SendKey,
    #[serde(default)]
    widget: WidgetCfg,
    #[serde(default)]
    firmware: FirmwareCfg,
}

/// 语音模型状态(悬浮窗/主面板展示)。
pub mod asr_state {
    pub const MISSING: &str = "missing";
    pub const DOWNLOADING: &str = "downloading";
    pub const READY: &str = "ready";
    pub const RECOGNIZING: &str = "recognizing";
}

#[derive(Debug, Clone, Serialize)]
pub struct AppState {
    pub connected: bool,
    pub battery: Option<u8>,
    /// 设备固件版本(STATUS_NOTIFY.fwVer);UP/DOWN 的 PC 分发需 ≥ 0x0002。
    pub fw_ver: u16,
    /// OK 按住中(录音中)。
    pub ptt: bool,
    /// asr_state::* 或 "error: ..."。
    pub asr: String,
    /// 模型下载进度 0..100。
    pub download_pct: u8,
    /// 最近一次成功键入的文本(主面板"最近识别"展示;悬浮窗只做状态指示,不显示文本)。
    pub last_text: Option<String>,
    pub send_key: SendKey,
    /// 固件烧录设置(镜像路径/波特率/是否回读校验)。
    pub firmware: FirmwareCfg,
    /// 烧录清空了设备侧 NVS,需要用户去系统蓝牙设置里重新配对一次。
    /// 面板上只据此显示"蓝牙需要重新配对 + 打开蓝牙设置"这一块;连上设备后自动转 false。
    pub pairing_needed: bool,
}

pub struct Shared {
    pub state: Mutex<AppState>,
    pub widget: Mutex<WidgetCfg>,
    pub config_path: PathBuf,
    config_write: Mutex<()>,
    model_task: Arc<AtomicBool>,
}

/// 下载/重置/识别共用模型文件,与可变的 UI 状态独立互斥。
pub struct ModelTask(Arc<AtomicBool>);

impl Drop for ModelTask {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Shared {
    pub fn new() -> Self {
        let config_path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("vibemic")
            .join("config.json");
        Shared {
            state: Mutex::new(AppState {
                connected: false,
                battery: None,
                fw_ver: 0,
                ptt: false,
                asr: asr_state::MISSING.into(),
                download_pct: 0,
                last_text: None,
                send_key: SendKey::Enter,
                firmware: FirmwareCfg::default(),
                pairing_needed: false,
            }),
            widget: Mutex::new(WidgetCfg::default()),
            config_path,
            config_write: Mutex::new(()),
            model_task: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn try_model_task(&self) -> Option<ModelTask> {
        self.model_task
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| ModelTask(self.model_task.clone()))
    }

    /// 状态锁。中毒(某次 panic 时正持锁)后继续用内部数据:一个 panic 不该
    /// 让之后所有状态更新与广播全部永久 `.unwrap()` 失败。
    pub fn state_guard(&self) -> MutexGuard<'_, AppState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 悬浮窗几何锁,中毒处理同 [`Shared::state_guard`]。
    pub fn widget_guard(&self) -> MutexGuard<'_, WidgetCfg> {
        self.widget
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 固件烧录设置更新:与 [`Shared::update_widget`] 同规矩 —— 锁只收紧在改内存的
    /// 那一小段里,落盘在锁外完成(`save_config` 自己还要取 state/widget 两把锁)。
    pub fn update_firmware(&self, f: impl FnOnce(&mut FirmwareCfg)) -> Result<(), String> {
        {
            let mut s = self.state_guard();
            f(&mut s.firmware);
        }
        self.save_config()
    }

    pub fn load_config(&self) {
        let Ok(text) = std::fs::read_to_string(&self.config_path) else {
            return;
        };
        // 先按 ConfigFile 直解;失败则按宽松方式只抽 widget/firmware 字段(容忍多余的键)
        if let Ok(cfg) = serde_json::from_str::<ConfigFile>(&text) {
            {
                let mut s = self.state_guard();
                s.send_key = cfg.send_key;
                s.firmware = cfg.firmware;
            }
            *self.widget_guard() = cfg.widget;
            return;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(w) = v.get("widget").cloned() {
                if let Ok(w) = serde_json::from_value::<WidgetCfg>(w) {
                    *self.widget_guard() = w;
                }
            }
            if let Some(f) = v.get("firmware").cloned() {
                if let Ok(f) = serde_json::from_value::<FirmwareCfg>(f) {
                    self.state_guard().firmware = f;
                }
            }
        }
    }

    /// 悬浮窗几何/固定状态更新:锁只在改内存的一小段里持有,落盘在锁外完成。
    ///
    /// 「改内存 → 调 save_config」必须走这里,不要在调用方自己 `let g =
    /// self.widget.lock()` 之后再调 save_config:std 的 `Mutex` **不可重入**,
    /// 同一线程二次 `lock()` 会永久阻塞(不是 panic,是死等)。save_config 自己要
    /// 依次拿 state、widget 两把锁,于是「持 widget 锁调 save_config」= 自锁,
    /// 而这几个命令又跑在 UI 主线程上 → 整个进程的消息泵停摆,表现为
    /// 「拖动/点击悬浮窗后软件卡死无响应」。
    pub fn update_widget(&self, f: impl FnOnce(&mut WidgetCfg)) -> Result<(), String> {
        {
            let mut w = self.widget_guard();
            f(&mut w);
        }
        self.save_config()
    }

    /// 整份落盘(阻塞的文件写)。
    ///
    /// **调用方不得持有 `state` / `widget` 任何一把锁**:本函数内部会依次
    /// `state` 、`widget` 取锁,拿好了再进来就是同线程重入死锁
    /// (见 [`Shared::update_widget`])。
    ///
    /// 写法:先写临时文件再 rename —— 直接覆盖写如果中途崩溃/断电,会留下半截
    /// JSON,下次启动 load_config 静默回退默认值(用户设置的确认键与悬浮窗位置
    /// 一起丢)。
    pub fn save_config(&self) -> Result<(), String> {
        // 在取快照前串行化:旧快照不能在更新后的快照发布完毕后反向覆盖。
        let _write = self.config_write.lock().unwrap_or_else(|e| e.into_inner());
        let (send_key, firmware) = {
            let s = self.state_guard();
            (s.send_key, s.firmware.clone())
        };
        let widget = *self.widget_guard();
        if let Some(dir) = self.config_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
        }
        let cfg = ConfigFile {
            send_key,
            widget,
            firmware,
        };
        let text = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
        let tmp = self.config_path.with_extension("json.tmp");
        let result = (|| {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&tmp, &self.config_path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(|e| format!("保存配置失败: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    /// 测试用 Shared:配置写到临时目录,绝不碰用户真实的 config.json。
    fn temp_shared(tag: &str) -> Shared {
        let mut s = Shared::new();
        s.config_path =
            std::env::temp_dir().join(format!("vibemic-test-{tag}-{}.json", std::process::id()));
        s
    }

    #[test]
    fn concurrent_saves_publish_complete_latest_config() {
        let shared = Arc::new(temp_shared("concurrent"));
        shared.save_config().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let s = shared.clone();
        let r = running.clone();
        let reader = std::thread::spawn(move || {
            while r.load(Ordering::Relaxed) {
                let bytes = std::fs::read(&s.config_path).unwrap();
                serde_json::from_slice::<ConfigFile>(&bytes).expect("published partial JSON");
            }
        });
        let workers: Vec<_> = (0..8)
            .map(|id| {
                let s = shared.clone();
                std::thread::spawn(move || {
                    for round in 0..50 {
                        s.update_firmware(|f| {
                            f.path = format!("{id}-{round}-{}", "x".repeat(2048 + id * 100))
                        })
                        .unwrap();
                        s.update_widget(|w| w.x += 1.0).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        running.store(false, Ordering::Relaxed);
        reader.join().unwrap();
        let cfg: ConfigFile =
            serde_json::from_slice(&std::fs::read(&shared.config_path).unwrap()).unwrap();
        assert_eq!(cfg.widget.x, shared.widget_guard().x);
        assert_eq!(cfg.widget.x, default_widget_x() + 400.0);
        assert_eq!(cfg.firmware.path, shared.state_guard().firmware.path);
        std::fs::remove_file(&shared.config_path).unwrap();
    }

    #[test]
    fn persistence_failure_is_reported_without_destroying_config() {
        let shared = temp_shared("failure");
        shared.save_config().unwrap();
        let previous = std::fs::read(&shared.config_path).unwrap();
        let tmp = shared.config_path.with_extension("json.tmp");
        std::fs::create_dir(&tmp).unwrap(); // make temp-file creation fail
        assert!(shared.update_widget(|w| w.x = 42.0).is_err());
        assert_eq!(std::fs::read(&shared.config_path).unwrap(), previous);
        std::fs::remove_dir(&tmp).unwrap();
        std::fs::remove_file(&shared.config_path).unwrap();
    }

    #[test]
    fn model_task_guard_survives_ui_changes_and_releases_on_unwind() {
        let shared = Shared::new();
        let task = shared.try_model_task().unwrap();
        for status in [asr_state::RECOGNIZING, "error: missing", asr_state::READY] {
            shared.state_guard().asr = status.into();
            assert!(shared.try_model_task().is_none());
        }
        drop(task);
        let result = std::panic::catch_unwind(|| {
            let _task = shared.try_model_task().unwrap();
            panic!("worker failed");
        });
        assert!(result.is_err());
        assert!(shared.try_model_task().is_some());
    }

    /// `update_widget` 必须在内部把 widget 锁收紧之后再调 save_config:若持锁调,
    /// std::sync::Mutex 不可重入,在主线程上就是永久死锁 → 拖动/点击悬浮窗后整个
    /// 软件无响应。看门狗线程把「挂住」变成断言失败,而不是让测试永远卡住。
    #[test]
    fn update_widget_does_not_self_lock() {
        let shared = Arc::new(temp_shared("widget"));
        let (tx, rx) = mpsc::channel();
        let s2 = shared.clone();
        std::thread::spawn(move || {
            s2.update_widget(|w| {
                w.x = 42.0;
                w.y = 43.0;
                w.pinned = true;
            })
            .unwrap();
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "update_widget 5s 未返回:又变成持 widget 锁调 save_config 的自锁了"
        );
        let w = *shared.widget.lock().unwrap();
        assert_eq!((w.x, w.y, w.pinned), (42.0, 43.0, true), "内存没更新");
        // 真的落盘了才算数(前端 await 的命令,静默不写盘也是 bug)
        let text = std::fs::read_to_string(&shared.config_path).expect("config.json 未落盘");
        assert!(text.contains("\"x\": 42.0"), "落盘内容不对: {text}");
        let _ = std::fs::remove_file(&shared.config_path);
    }

    /// 整份保存(send_key 那条路径)同样不许在持锁状态下调用:先放锁、再落盘。
    #[test]
    fn save_config_after_releasing_locks() {
        let shared = Arc::new(temp_shared("sendkey"));
        let (tx, rx) = mpsc::channel();
        let s2 = shared.clone();
        std::thread::spawn(move || {
            {
                let mut s = s2.state.lock().unwrap();
                s.send_key = SendKey::CtrlEnter;
            }
            s2.save_config().unwrap();
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "save_config 5s 未返回:调用方持着 state/widget 锁"
        );
        let text = std::fs::read_to_string(&shared.config_path).expect("config.json 未落盘");
        assert!(text.contains("ctrl_enter"), "落盘内容不对: {text}");
        let _ = std::fs::remove_file(&shared.config_path);
    }
}
