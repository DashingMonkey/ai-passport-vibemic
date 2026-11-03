//! 轻量文件诊断日志:%APPDATA%/vibemic/app.log(与 config.json 同目录)。
//!
//! 托盘应用没有可见的 stdout/eprintln 出口,"按了没反应"这类问题需要事后
//! 翻日志定位。时间戳用进程启动后的秒数(免引时间库,排查时序足够)。

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

static LOCK: Mutex<()> = Mutex::new(());
static START: Mutex<Option<Instant>> = Mutex::new(None);
/// 本进程已写入的字节数:长会话(链路反复重连)也要能滚动,不能只在启动时截断。
static BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const MAX_LOG_BYTES: u64 = 1_000_000;

fn log_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("vibemic")
        .join("app.log")
}

/// 启动时调用一次:建目录(OpenOptions::create 不建父目录,不建则首条日志
/// 静默丢失),超 1MB 截断。
pub fn init() {
    let p = log_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut start_bytes = 0;
    if let Ok(meta) = std::fs::metadata(&p) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::write(&p, b"");
        } else {
            start_bytes = meta.len();
        }
    }
    BYTES.store(start_bytes, std::sync::atomic::Ordering::Relaxed);
    log("---- app start ----");
}

pub fn log(msg: &str) {
    // 锁中毒(某次 panic 时正持锁)不该让日志永久失效。
    let mut start = START
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if start.is_none() {
        *start = Some(Instant::now());
    }
    let secs = start.unwrap().elapsed().as_secs_f32();
    drop(start);

    let _g = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let p = log_path();
    // 会话中途也要滚动:超限就改名为 app.log.1 重开,保留上一段现场。
    let written = BYTES.load(std::sync::atomic::Ordering::Relaxed);
    if written > MAX_LOG_BYTES {
        let _ = std::fs::rename(&p, p.with_extension("log.1"));
        BYTES.store(0, std::sync::atomic::Ordering::Relaxed);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&p) {
        let line = format!("[{secs:>9.2}s] {msg}\n");
        if writeln!(f, "{}", line.trim_end()).is_ok() {
            BYTES.fetch_add(line.len() as u64, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
