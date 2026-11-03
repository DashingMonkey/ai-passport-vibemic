//! 设备通道抽象:BLE 自定义服务优先,USB 串口兜底。
//! 帧格式与通道无关(`vibemic_protocol::frame`);串口通道每帧后补 `\n` 分隔。
//! Windows 上 `WinRtLink` 由调用方同进程使用(设备线程里直接跑 WinRT)。

pub mod ble;
pub mod serial;

#[cfg(target_os = "windows")]
pub mod winrt;

use std::fmt;
use std::time::Duration;

use vibemic_protocol::frame::Frame;

pub use ble::{scan_ble, BleLink, ScanHit};
pub use serial::{SerialLink, DEFAULT_BAUD};

// ---- 可选日志出口 ----

/// 链路层诊断出口(全局唯一,宿主注册一次)。
///
/// 为什么要这个钩子:桌面应用是托盘程序,没有可见的 stdout —— 链路内部的诊断
/// 信息(服务发现重试到第几次、哪个特征值的 CCCD 写不进去)只走 `eprintln!`
/// 在 GUI 进程里等于直接丢弃,日志里只剩一句 `cccd write unreachable`,看不出
/// 断在 VM_STATUS 还是 VM_AUDIO。类型用函数指针而不是 `Fn`/闭包:调用点分布在
/// WinRT 事件回调里,这里不引入装箱、泛型参数或额外锁。
static LOGGER: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

/// 注册日志出口(重复调用忽略;不注册则退回 stderr,vmctl 这类 CLI 行为不变)。
pub fn set_logger(f: fn(&str)) {
    let _ = LOGGER.set(f);
}

/// 链路层诊断输出(库内部使用)。
pub(crate) fn log(msg: &str) {
    match LOGGER.get() {
        Some(f) => f(msg),
        None => eprintln!("{msg}"),
    }
}

#[cfg(target_os = "windows")]
pub use winrt::WinRtLink;

#[derive(Debug)]
pub enum LinkError {
    /// 未发现/未连接目标设备。
    NotFound(String),
    /// 超时。
    Timeout,
    /// 传输层 I/O 错误。
    Io(String),
    /// 平台或配置不支持。
    Unsupported(String),
    /// 协议层错误(帧不合法等)。
    Protocol(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::NotFound(m) => write!(f, "device not found: {m}"),
            LinkError::Timeout => write!(f, "timeout"),
            LinkError::Io(m) => write!(f, "io error: {m}"),
            LinkError::Unsupported(m) => write!(f, "unsupported: {m}"),
            LinkError::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for LinkError {}

/// 控制面通道:发命令帧(VM_CTRL / 串口),收上行帧(通知 / 串口回帧)。
/// 实现必须是 `Send`;方法可能阻塞(内部各自处理异步运行时)。
pub trait DeviceLink: Send {
    fn name(&self) -> &'static str;

    /// 发送一帧到下行通道(VM_CTRL 特征值 / 串口)。
    fn send(&mut self, frame: &Frame) -> Result<(), LinkError>;

    /// 等待下一个上行帧;超时返回 `Ok(None)`。
    fn poll(&mut self, timeout: Duration) -> Result<Option<Frame>, LinkError>;
}

/// 循环收帧直到谓词匹配或总超时(沿途帧被丢弃,只用于单发单收场景)。
/// 独立成自由函数以保持 `DeviceLink` dyn 兼容。
pub fn wait_for(
    link: &mut dyn DeviceLink,
    timeout: Duration,
    pred: &dyn Fn(&Frame) -> bool,
) -> Result<Option<Frame>, LinkError> {
    let deadline = std::time::Instant::now() + timeout;
    while let Some(remain) = deadline.checked_duration_since(std::time::Instant::now()) {
        match link.poll(remain)? {
            Some(f) if pred(&f) => return Ok(Some(f)),
            Some(_) => continue,
            None => return Ok(None),
        }
    }
    Ok(None)
}
