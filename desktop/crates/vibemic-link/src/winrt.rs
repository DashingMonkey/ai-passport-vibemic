//! Windows WinRT 直连通道:Windows 上的正式 BLE 路径。
//!
//! btleplug 的 Windows 后端只能发现"正在广播"的设备;而已配对的 HID 设备被 OS
//! 持续连接、不广播,扫描路径够不着。本模块按已知设备名经
//! `BluetoothLEDevice::FromIdAsync` 直接建立 GATT 会话 —— 与 OS 的 HID 消费
//! 共用同一条连接。
//!
//! 所有 WinRT 异步调用用阻塞 `.get()`;`DeviceLink` 方法本身即同步语义,由调用方
//! 在自己的线程里使用(vmctl 的 CLI 线程 / 桌面应用的 device-link 线程)。

#![cfg(target_os = "windows")]

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;
use windows::core::Ref;
use windows::Devices::Bluetooth::BluetoothCacheMode;
use windows::Devices::Bluetooth::BluetoothDeviceId;
use windows::Devices::Bluetooth::BluetoothLEDevice;
use windows::Devices::Enumeration::DeviceInformation;
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattSession,
    GattCharacteristic, GattClientCharacteristicConfigurationDescriptorValue,
    GattCommunicationStatus, GattDeviceService, GattValueChangedEventArgs, GattWriteOption,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, DataWriter};

use vibemic_protocol::frame::{frame_type, Decoder, Frame, FrameError};
use vibemic_protocol::uuids;

use crate::{DeviceLink, LinkError};

/// 服务枚举在新会话建立初期可能返回 Unreachable —— 重试至多 6 次。
const DISCOVER_RETRIES: u32 = 6;

thread_local! {
    /// 每个线程只初始化一次 COM 公寓。
    static COM_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// WinRT 异步 `.get()` 在未初始化 COM 的线程上可能死锁(尤其宿主进程主线程已是
/// STA 时)。MTA 初始化幂等;失败(例如 RPC_E_CHANGED_MODE:线程已属 STA)不致命,
/// 后续调用沿用宿主已有公寓。每个线程只尝试一次,避免反复重连累积公寓引用计数。
fn ensure_com_initialized() {
    COM_READY.with(|ready| {
        if ready.get() {
            return;
        }
        ready.set(true);
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
        }
    });
}

fn guid_of(uuid: &Uuid) -> windows::core::GUID {
    windows::core::GUID::from_u128(uuid.as_u128())
}

fn link_err(e: windows::core::Error) -> LinkError {
    LinkError::Io(format!("winrt: {e}"))
}

pub struct WinRtLink {
    _device: BluetoothLEDevice,
    // MaintainConnection 的 GattSession:设备重启重新广播后 OS 主动回连。
    // 没有它,FromIdAsync 成功但 ACL 未建立,CCCD 写要 Unreachable 数分钟。
    _session: windows::Devices::Bluetooth::GenericAttributeProfile::GattSession,
    ctrl_chr: GattCharacteristic,
    // GattDeviceService 必须 Close()(泄漏 service 对象会把 OS 级会话/挂起操作
    // 遗留给系统),持有以便 Drop 时关闭。
    svc: GattDeviceService,
    // (特征值, ValueChanged 注册 token):保活特征值对象(源头被 Drop 时注册
    // 随之失效),并在 Drop 时成对注销 —— 不注销则每次重连泄漏一组注册与
    // COM 引用。
    subs: Vec<(GattCharacteristic, Option<i64>)>,
    rx: Receiver<Frame>,
    last_session: Instant,
}

impl WinRtLink {
    /// 按设备名连接:枚举系统已配对/已知的 BLE 设备(BluetoothLEDevice 选择器),
    /// 匹配 VibeMic 设备名后 FromIdAsync —— 对"被 OS 持续连接、不广播"的
    /// 已配对设备同样有效(FromBluetoothAddressAsync 无论哪个字节序都返回 null)。
    pub fn connect() -> Result<WinRtLink, LinkError> {
        ensure_com_initialized();
        let selector = BluetoothLEDevice::GetDeviceSelector().map_err(link_err)?;
        let collection = DeviceInformation::FindAllAsyncAqsFilter(&selector)
            .map_err(link_err)?
            .get()
            .map_err(link_err)?;
        let count = collection.Size().map_err(link_err)?;
        let mut device_id = None;
        // 没匹配上时把候选名字带进错误信息:否则"连不上"只能看到一句 not paired,
        // 而真正的原因往往是系统缓存里的旧名或设备名被改过。
        let mut candidates: Vec<String> = Vec::new();
        for i in 0..count {
            let info = collection.GetAt(i).map_err(link_err)?;
            let name = info.Name().map_err(link_err)?.to_string();
            if uuids::device_name_matches(&name) {
                device_id = Some(info.Id().map_err(link_err)?.to_string());
                break;
            }
            if !name.is_empty() && candidates.len() < 8 {
                candidates.push(name);
            }
        }
        let id = device_id.ok_or_else(|| {
            LinkError::NotFound(format!(
                "'{}' not paired on this PC (candidates: {})",
                uuids::DEVICE_NAME,
                if candidates.is_empty() {
                    "none".to_string()
                } else {
                    candidates.join(", ")
                }
            ))
        })?;
        let device = BluetoothLEDevice::FromIdAsync(&windows::core::HSTRING::from(&id))
            .map_err(link_err)?
            .get()
            .map_err(link_err)?;
        // 让 OS 只要设备在广播就保持连接:CCCD 写不再等 ACL 自然重建。
        let session =
            GattSession::FromDeviceIdAsync(&BluetoothDeviceId::FromId(&windows::core::HSTRING::from(&id))
                .map_err(link_err)?)
            .map_err(link_err)?
            .get()
            .map_err(link_err)?;
        session.SetMaintainConnection(true).map_err(link_err)?;

        let link = finish_connect(&device, &session);
        if link.is_err() {
            // 失败路径显式 Close:Make 出来的会话/设备对象不主动关闭会把
            // MaintainConnection 请求与挂起的 GATT 操作遗留给系统累积。
            let _ = session.Close();
            let _ = device.Close();
        }
        link
    }
}

// ---- 连接辅助 ----

/// 撤销本地回调与远端 CCCD,包含订阅中途失败的当前特征值,然后 Close service。
/// `Drop for WinRtLink` 只在成功构造后被析构时才跑,不覆盖构造中途的失败路径。
fn rollback_subscriptions(
    registered: &mut Vec<(GattCharacteristic, Option<i64>)>,
    svc: &GattDeviceService,
) {
    for (chr, token) in registered.drain(..) {
        if let Some(token) = token {
            let _ = chr.RemoveValueChanged(token);
        }
        // 仅注销回调不会撤销远端 CCCD。失败仍继续清理其余特征值;
        // 设备端会话租约兜底进程崩溃、断电或写入失败的情况。
        let result = chr
            .WriteClientCharacteristicConfigurationDescriptorAsync(
                GattClientCharacteristicConfigurationDescriptorValue::None)
            .and_then(|op| {
                let deadline = Instant::now() + Duration::from_secs(2);
                // AsyncStatus::Started = 0. Bound cleanup when the radio is gone.
                while op.Status()?.0 == 0 {
                    if Instant::now() >= deadline {
                        let _ = op.Cancel();
                        return Err(windows::core::Error::new(
                            windows::core::HRESULT(0x800705b4u32 as i32), "CCCD cleanup timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                op.get()
            });
        match result {
            Ok(GattCommunicationStatus::Success) => {},
            Ok(status) => crate::log(&format!("[winrt] unsubscribe: {status:?}")),
            Err(e) => crate::log(&format!("[winrt] unsubscribe failed: {e}")),
        }
    }
    let _ = svc.Close();
}

fn finish_connect(
    device: &BluetoothLEDevice,
    session: &GattSession,
) -> Result<WinRtLink, LinkError> {
        let svc_uuid = guid_of(&Uuid::parse_str(uuids::SERVICE).unwrap());
        // 发现策略(Windows GATT 缓存分两层):
        // ① 缓存的服务表可能整表陈旧(固件加了特征值)——特征值找不齐就得
        //    换 Uncached 重新发现**服务对象**,光在旧对象上换缓存模式问特征值
        //    会返回空列表;
        // ② Uncached 发现要数秒且可能落空,重试 DISCOVER_RETRIES 次。
        // 只有 ctrl/status/event 三个必备都找齐才算发现成功(音频可选)。
        let find_on = |service: &GattDeviceService,
                       want: &str|
         -> Option<GattCharacteristic> {
            let guid = guid_of(&Uuid::parse_str(want).unwrap());
            for cache in [BluetoothCacheMode::Cached, BluetoothCacheMode::Uncached] {
                let Ok(result) = service.GetCharacteristicsWithCacheModeAsync(cache) else {
                    continue;
                };
                let Ok(result) = result.get() else { continue };
                let Ok(chars) = result.Characteristics() else { continue };
                if let Some(c) = chars
                    .into_iter()
                    .find(|c| c.Uuid().map(|u| u == guid).unwrap_or(false))
                {
                    return Some(c);
                }
            }
            None
        };

        let mut ctrl = None;
        let mut status = None;
        let mut event = None;
        let mut audio = None;
        // 命中的服务对象:持有它才能在 Drop 时 Close(不泄漏给系统)。
        let mut svc_obj: Option<GattDeviceService> = None;
        'discovery: for (svc_cache, retries) in [
            (BluetoothCacheMode::Cached, 1u32),
            (BluetoothCacheMode::Uncached, DISCOVER_RETRIES),
        ] {
            for attempt in 1..=retries {
                let result = device
                    .GetGattServicesWithCacheModeAsync(svc_cache)
                    .map_err(link_err)?
                    .get()
                    .map_err(link_err)?;
                let services = result.Services().map_err(link_err)?;
                let hit = services.into_iter().find(|s| {
                    s.Uuid().map(|u| u == svc_uuid).unwrap_or(false)
                });
                let Some(service) = hit else {
                    crate::log(&format!(
                        "[winrt] service discovery retry {attempt}/{retries} ({svc_cache:?})"
                    ));
                    std::thread::sleep(Duration::from_millis(1000));
                    continue;
                };
                ctrl = find_on(&service, uuids::CHR_VM_CTRL);
                status = find_on(&service, uuids::CHR_VM_STATUS);
                event = find_on(&service, uuids::CHR_VM_EVENT);
                audio = find_on(&service, uuids::CHR_VM_AUDIO);
                if ctrl.is_some() && status.is_some() && event.is_some() {
                    svc_obj = Some(service);
                    break 'discovery;
                }
                crate::log(&format!(
                    "[winrt] characteristics incomplete (ctrl={:?}), retry {attempt}/{retries} ({svc_cache:?})",
                    ctrl.is_some()
                ));
                // 这个服务对象连同它上面的特征值一起作废:显式 Close 后再重试,
                // 否则每轮重试都把一个 GattDeviceService 泄漏给系统。
                ctrl = None;
                status = None;
                event = None;
                audio = None;
                let _ = service.Close();
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
        let ctrl_chr = ctrl.ok_or_else(|| LinkError::Protocol("VM_CTRL not found".into()))?;
        let status_chr =
            status.ok_or_else(|| LinkError::Protocol("VM_STATUS not found".into()))?;
        let event_chr =
            event.ok_or_else(|| LinkError::Protocol("VM_EVENT not found".into()))?;
        let svc = svc_obj.ok_or_else(|| LinkError::Protocol("VM service not found".into()))?;
        // 音频特征值可选(设备可能不提供 0x7a05):缺了不阻断控制面。
        let audio_chr = audio;

        // 订阅通知(ValueChanged → 解帧/音频块 → channel)。
        // 顺序:先 CCCD 后注册——CCCD 失败直接回滚返回(无注册需清理),注册
        // 成功的 (特征值, token) 成对收集,Drop 时统一注销。
        let (tx, rx): (Sender<Frame>, Receiver<Frame>) = std::sync::mpsc::channel();
        // 带名字的订阅清单:订阅失败时必须能说出是哪个特征值断的(只报一句
        // "cccd write unreachable"无法分辨断在 VM_STATUS 还是 VM_AUDIO)。
        let mut to_subscribe: Vec<(GattCharacteristic, bool, &'static str)> = vec![
            (status_chr, false, "VM_STATUS"),
            (event_chr, false, "VM_EVENT"),
        ];
        if let Some(a) = audio_chr {
            to_subscribe.push((a, true, "VM_AUDIO"));
        }
        let mut registered: Vec<(GattCharacteristic, Option<i64>)> = Vec::new();
        for (chr, is_audio, label) in to_subscribe {
            // 在第一次远端写之前记录,包含写入结果不明及本地 handler 注册失败。
            registered.push((chr.clone(), None));
            // Windows 按"OS 缓存的 CCCD 值"去重 CCCD 写:设备重启后 CCCD 已清零
            // 而缓存仍为 Notify 时,单写 Notify 会被跳过。
            // 先写 None(与缓存不同 → 真正下发,同时刷新缓存)再写 Notify,
            // 两次都强制到达设备,消除失同步。
            // FromIdAsync 后链路可能尚未就绪(缓存路径尤其快),Unreachable 重试。
            let mut ok = false;
            for _ in 0..5 {
                let mut failed = false;
                for value in [
                    GattClientCharacteristicConfigurationDescriptorValue::None,
                    GattClientCharacteristicConfigurationDescriptorValue::Notify,
                ] {
                    let status = match chr
                        .WriteClientCharacteristicConfigurationDescriptorAsync(value)
                        .and_then(|op| op.get())
                    {
                        Ok(s) => s,
                        Err(e) => {
                            crate::log(&format!("[winrt] {label} cccd get failed: {e}"));
                            rollback_subscriptions(&mut registered, &svc);
                            return Err(link_err(e));
                        }
                    };
                    if status != GattCommunicationStatus::Success {
                        crate::log(&format!("[winrt] {label} cccd {status:?},重试"));
                        failed = true;
                        break;
                    }
                }
                if !failed {
                    ok = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(800));
            }
            if !ok {
                crate::log(&format!(
                    "[winrt] {label} 订阅失败,撤销本次尝试的 {} 个特征值",
                    registered.len()
                ));
                rollback_subscriptions(&mut registered, &svc);
                return Err(LinkError::Io("cccd write unreachable".into()));
            }
            let tx = tx.clone();
            // 非音频特征值走流式解码:一条通知可能含多帧(帧也可能跨通知),
            // 逐字节喂解码器,凑齐多少帧就转发多少帧 —— 逐条通知整帧解码会在
            // 载荷超过单包时静默丢帧。
            let decoder = if is_audio {
                None
            } else {
                Some(Arc::new(Mutex::new(Decoder::new())))
            };
            let handler = TypedEventHandler::new(
                move |_: Ref<GattCharacteristic>, args: Ref<GattValueChangedEventArgs>| {
                    if let Ok(args) = args.ok() {
                        if let Ok(value) = args.CharacteristicValue() {
                            if let Ok(reader) = DataReader::FromBuffer(&value) {
                                let len =
                                    reader.UnconsumedBufferLength().unwrap_or(0) as usize;
                                let mut buf = vec![0u8; len];
                                if reader.ReadBytes(&mut buf).is_ok() {
                                    if is_audio {
                                        // 音频块不是协议帧:包装成伪帧转发,
                                        // 重组/解码见 vibemic-protocol::audio。
                                        let _ = tx.send(Frame {
                                            version: 0,
                                            kind: vibemic_protocol::audio::AUDIO_CHUNK,
                                            payload: buf,
                                        });
                                    } else if let Some(decoder) = decoder.as_ref() {
                                        let mut decoder = decoder
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                                        for &byte in &buf {
                                            if let Some(frame) = decoder.feed(byte) {
                                                let _ = tx.send(frame);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                },
            );
            let token = match chr.ValueChanged(&handler) {
                Ok(token) => token,
                Err(e) => {
                    rollback_subscriptions(&mut registered, &svc);
                    return Err(link_err(e));
                }
            };
            // handler 的 Rust 对象到此可自然 Drop:WinRT 事件源对已注册的
            // delegate 持强引用,保活由注册本身完成。
            registered.last_mut().unwrap().1 = Some(token);
        }

        let mut link = WinRtLink {
            _device: device.clone(),
            _session: session.clone(),
            ctrl_chr,
            svc,
            subs: registered,
            rx,
            last_session: Instant::now(),
        };
        // 完成全部订阅后才激活应用会话;旧固件会忽略此扩展命令。
        link.send(&Frame::new(frame_type::CMD_PC_SESSION, [1]))?;
        Ok(link)
}

impl Drop for WinRtLink {
    fn drop(&mut self) {
        // 注销 → 逐层 Close,顺序与 bleak 的 disconnect() 一致(处理器 →
        // service → session → device)。COM 引用释放不会取消挂起的 GATT
        // 操作,显式 Close 才会;不这么做,失败/重连路径遗留的
        // MaintainConnection 会话会持续累积压垮蓝牙栈。
        // bleak 的 hack:GattDeviceService.Close() 偶发永久挂起,调用前
        // 短暂停歇规避;该 Drop 只在断连/退出路径执行,阻塞可接受。
        std::thread::sleep(Duration::from_millis(100));
        rollback_subscriptions(&mut self.subs, &self.svc);
        let _ = self._session.Close();
        let _ = self._device.Close();
    }
}

impl DeviceLink for WinRtLink {
    fn name(&self) -> &'static str {
        "winrt"
    }

    fn send(&mut self, frame: &Frame) -> Result<(), LinkError> {
        let bytes = frame.encode().map_err(|e: FrameError| LinkError::Protocol(format!("{e:?}")))?;
        let writer = DataWriter::new().map_err(link_err)?;
        writer.WriteBytes(&bytes).map_err(link_err)?;
        let buffer = writer.DetachBuffer().map_err(link_err)?;
        let status = self
            .ctrl_chr
            .WriteValueWithOptionAsync(&buffer, GattWriteOption::WriteWithResponse)
            .map_err(link_err)?
            .get()
            .map_err(link_err)?;
        if status == GattCommunicationStatus::Success {
            Ok(())
        } else {
            Err(LinkError::Io(format!("write VM_CTRL: {status:?}")))
        }
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
                    return Err(LinkError::Io("winrt event channel closed".into()));
                }
            }
        }
    }
}
