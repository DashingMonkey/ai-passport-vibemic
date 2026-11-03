//! BLE 通道(btleplug 0.13):扫描/连接 `AI Passport Vibe Mic`,写 VM_CTRL,订阅
//! VM_STATUS/VM_EVENT 通知。
//!
//! 注意:Windows 上被 OS 作为 HID 持续连接的已配对设备**不广播**,btleplug 枚举
//! 不到 —— 正式路径是 `winrt::WinRtLink`(同进程使用);本模块用于"设备正在广播"
//! 的场景(扫描、非 Windows 后备)。
//!
//! 并发模型:一个后台多线程 tokio runtime 常驻泵取通知流;命令经同一 runtime
//! 的句柄 `block_on` 执行,与泵并发。

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::Manager;
use futures::StreamExt;
use tokio::runtime::Runtime;
use uuid::Uuid;

use vibemic_protocol::frame::{frame_type, Frame, FrameError};
use vibemic_protocol::uuids;

use crate::{DeviceLink, LinkError};

const RUNTIME_WORKERS: usize = 2;

struct ConstUuids {
    service: Uuid,
    ctrl: Uuid,
    profile: Uuid,
    status: Uuid,
    event: Uuid,
}

fn const_uuids() -> &'static ConstUuids {
    static CELL: OnceLock<ConstUuids> = OnceLock::new();
    CELL.get_or_init(|| ConstUuids {
        service: Uuid::parse_str(uuids::SERVICE).expect("service uuid"),
        ctrl: Uuid::parse_str(uuids::CHR_VM_CTRL).expect("ctrl uuid"),
        profile: Uuid::parse_str(uuids::CHR_VM_PROFILE).expect("profile uuid"),
        status: Uuid::parse_str(uuids::CHR_VM_STATUS).expect("status uuid"),
        event: Uuid::parse_str(uuids::CHR_VM_EVENT).expect("event uuid"),
    })
}

/// 扫描结果条目。
#[derive(Debug, Clone)]
pub struct ScanHit {
    pub id: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub vibemic: bool, // 广播含 VibeMic 服务 UUID 或设备名匹配
}

fn spawn_runtime() -> Result<Arc<Runtime>, LinkError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(RUNTIME_WORKERS)
        .thread_name("vibemic-ble")
        .enable_all()
        .build()
        .map_err(|e| LinkError::Io(format!("create runtime: {e}")))?;
    Ok(Arc::new(rt))
}

fn map_frame_error(e: FrameError) -> LinkError {
    LinkError::Protocol(format!("{e:?}"))
}

/// 扫描 `seconds` 秒,返回可见外设(VibeMic 命中排前)。
pub fn scan_ble(seconds: Duration) -> Result<Vec<ScanHit>, LinkError> {
    let rt = spawn_runtime()?;
    rt.block_on(async move {
        let manager = Manager::new()
            .await
            .map_err(|e| LinkError::Io(e.to_string()))?;
        let adapters = manager
            .adapters()
            .await
            .map_err(|e| LinkError::Io(e.to_string()))?;
        let adapter = adapters
            .into_iter()
            .next()
            .ok_or_else(|| LinkError::Unsupported("no bluetooth adapter".into()))?;
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|e| LinkError::Io(format!("start scan: {e}")))?;
        tokio::time::sleep(seconds).await;

        let mut hits = Vec::new();
        let periphs = adapter
            .peripherals()
            .await
            .map_err(|e| LinkError::Io(e.to_string()))?;
        for p in periphs {
            let props = p.properties().await.ok().flatten();
            let name = props.as_ref().and_then(|x| x.local_name.clone());
            let vibemic = name
                .as_deref()
                .map(uuids::device_name_matches)
                .unwrap_or(false)
                || props
                    .as_ref()
                    .map(|x| x.services.contains(&const_uuids().service))
                    == Some(true);
            if !vibemic && name.is_none() {
                continue; // 未具名的无关外设
            }
            hits.push(ScanHit {
                id: p.address().to_string(),
                name,
                rssi: props.and_then(|x| x.rssi),
                vibemic,
            });
        }
        let _ = adapter.stop_scan().await;
        hits.sort_by(|a, b| b.vibemic.cmp(&a.vibemic).then(a.id.cmp(&b.id)));
        Ok(hits)
    })
}

/// BLE 通道实例。
pub struct BleLink {
    last_session: Instant,
    rt: Arc<Runtime>,
    peripheral: Arc<btleplug::platform::Peripheral>,
    ctrl_chr: Characteristic,
    profile_chr: Characteristic,
    rx: Receiver<Frame>,
}

impl BleLink {
    /// 扫描(最多 `timeout`)并连接第一个 VibeMic 设备,发现服务并订阅通知。
    pub fn connect(timeout: Duration) -> Result<BleLink, LinkError> {
        let rt = spawn_runtime()?;
        let (tx, rx) = std::sync::mpsc::channel::<Frame>();

        let periph = rt.block_on(async {
            let manager = Manager::new()
                .await
                .map_err(|e| LinkError::Io(e.to_string()))?;
            let adapters = manager
                .adapters()
                .await
                .map_err(|e| LinkError::Io(e.to_string()))?;
            let adapter = adapters
                .into_iter()
                .next()
                .ok_or_else(|| LinkError::Unsupported("no bluetooth adapter".into()))?;
            adapter
                .start_scan(ScanFilter::default())
                .await
                .map_err(|e| LinkError::Io(format!("start scan: {e}")))?;

            let deadline = tokio::time::Instant::now() + timeout;
            let mut target = None;
            while tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let periphs = adapter
                    .peripherals()
                    .await
                    .map_err(|e| LinkError::Io(e.to_string()))?;
                for p in periphs {
                    let props = p.properties().await.ok().flatten();
                    let name = props.as_ref().and_then(|x| x.local_name.clone());
                    let has_svc = props
                        .as_ref()
                        .map(|x| x.services.contains(&const_uuids().service));
                    if name
                        .as_deref()
                        .map(uuids::device_name_matches)
                        .unwrap_or(false)
                        || has_svc == Some(true)
                    {
                        target = Some(p);
                        break;
                    }
                }
                if target.is_some() {
                    break;
                }
            }
            let _ = adapter.stop_scan().await;
            let p = target.ok_or_else(|| {
                LinkError::NotFound(format!(
                    "'{}' not seen advertising within {:?}",
                    uuids::DEVICE_NAME,
                    timeout
                ))
            })?;
            p.connect()
                .await
                .map_err(|e| LinkError::Io(format!("connect: {e}")))?;
            p.discover_services()
                .await
                .map_err(|e| LinkError::Io(format!("gatt discovery: {e}")))?;
            Ok(p)
        })?;
        let peripheral = Arc::new(periph);

        // 定位 VibeMic 特征值。
        let cu = const_uuids();
        let find = |want: &Uuid| -> Option<Characteristic> {
            peripheral
                .services()
                .iter()
                .filter(|s| s.uuid == cu.service)
                .flat_map(|s| s.characteristics.iter())
                .find(|c| &c.uuid == want)
                .cloned()
        };
        let ctrl_chr = find(&cu.ctrl)
            .ok_or_else(|| LinkError::Protocol("VM_CTRL characteristic not found".into()))?;
        let profile_chr = find(&cu.profile)
            .ok_or_else(|| LinkError::Protocol("VM_PROFILE characteristic not found".into()))?;
        let status_chr = find(&cu.status)
            .ok_or_else(|| LinkError::Protocol("VM_STATUS characteristic not found".into()))?;
        let event_chr = find(&cu.event)
            .ok_or_else(|| LinkError::Protocol("VM_EVENT characteristic not found".into()))?;

        // 订阅 + 泵任务:通知流 → 解帧 → std channel。
        let pump_periph = peripheral.clone();
        rt.block_on(async {
            pump_periph
                .subscribe(&status_chr)
                .await
                .map_err(|e| LinkError::Io(format!("subscribe VM_STATUS: {e}")))?;
            pump_periph
                .subscribe(&event_chr)
                .await
                .map_err(|e| LinkError::Io(format!("subscribe VM_EVENT: {e}")))?;
            Ok::<(), LinkError>(())
        })?;

        let mut notifications = rt
            .block_on(async { peripheral.notifications().await })
            .map_err(|e| LinkError::Io(format!("open notification stream: {e}")))?;
        rt.spawn(async move {
            while let Some(n) = notifications.next().await {
                if n.uuid != cu.status && n.uuid != cu.event {
                    continue;
                }
                // BLE 通道一个通知即一个完整帧(设备端按帧 notify)。
                if let Ok(frame) = Frame::decode(&n.value) {
                    let _ = tx.send(frame);
                }
            }
        });

        let mut link = BleLink {
            last_session: Instant::now(),
            rt,
            peripheral,
            ctrl_chr,
            profile_chr,
            rx,
        };
        link.send(&Frame::new(frame_type::CMD_PC_SESSION, [1]))?;
        Ok(link)
    }

    /// 写 profile 分块(VM_PROFILE,write-without-response)。
    pub fn write_profile_chunk(&mut self, frame: &Frame) -> Result<(), LinkError> {
        let bytes = frame.encode().map_err(map_frame_error)?;
        let p = self.peripheral.clone();
        let chr = self.profile_chr.clone();
        let rt = self.rt.clone();
        rt.block_on(async move {
            p.write(&chr, &bytes, WriteType::WithoutResponse)
                .await
                .map_err(|e| LinkError::Io(format!("write VM_PROFILE: {e}")))
        })
    }
}

impl DeviceLink for BleLink {
    fn name(&self) -> &'static str {
        "ble"
    }

    fn send(&mut self, frame: &Frame) -> Result<(), LinkError> {
        let bytes = frame.encode().map_err(map_frame_error)?;
        let p = self.peripheral.clone();
        let chr = self.ctrl_chr.clone();
        self.rt.clone().block_on(async move {
            p.write(&chr, &bytes, WriteType::WithResponse)
                .await
                .map_err(|e| LinkError::Io(format!("write VM_CTRL: {e}")))
        })
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Frame>, LinkError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.last_session.elapsed() >= Duration::from_secs(5) {
                self.send(&Frame::new(frame_type::CMD_PC_SESSION, [1]))?;
                self.last_session = Instant::now();
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            let renew_in = Duration::from_secs(5).saturating_sub(self.last_session.elapsed());
            match self.rx.recv_timeout(remain.min(renew_in)) {
                Ok(f) => return Ok(Some(f)),
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => return Ok(None),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(LinkError::Io("notification stream closed".into()));
                }
            }
        }
    }
}
