//! 设备链路线程:WinRtLink 常驻重连;语音流收集、识别、键入与按键分发。
//!
//! 控制模型:
//! - OK 按住 = 录音(AUDIO_CHUNK 流入缓冲),松开 = 识别 + 键入焦点
//! - OK 双击 = 删除:第二下按下即删一个字符,继续按住则连删到松手(这一下不录音)
//! - UP 单击 = 确认(Enter/Ctrl+Enter 按设置)——固件 ≥0x0002 时不直发、交此处
//! - DOWN 单击 = 取消(Esc)——同上
//! - DOWN 双击 = 停止,连发两记 Esc(「双击 Esc 中断」类软件,如 DSH)——不按版本门控
//! - OK LONG 等其它事件忽略(PTT 期间设备会伴随上报 LONG)
//!
//! 链路同进程:WinRT 调用直接跑在本进程的设备线程里,交付物只有一个 exe;
//! 下面的心跳看门狗把"万一真的挂住"从静默变成可见。

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::Emitter;
use vibemic_link::{DeviceLink, WinRtLink};
use vibemic_protocol::frame::{frame_type, Frame};
use vibemic_protocol::payload::{button_event, button_id, ButtonEvent, StatusNotify};

use crate::diag;
use crate::dispatch;
use crate::state::{asr_state, Shared};

/// OK 按住期间收集的 PCM(识别在松开后一次性做;非流式)。
static AUDIO_BUF: std::sync::Mutex<Vec<i16>> = std::sync::Mutex::new(Vec::new());
/// 是否正在收集(OK 按下到松开)。
static COLLECTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 单次按住可采集的最长样本数(90s @16kHz):RELEASE 事件万一丢失,缓冲不会无限增长。
const AUDIO_MAX_SAMPLES: usize = 16_000 * 90;
/// 语音识别工作队列深度:满了就丢新录音并记日志(宁可丢一次,也不要乱序出字)。
const ASR_QUEUE_DEPTH: usize = 2;

/// UP/DOWN 由 PC 分发所需的最低固件版本(设备侧同版本起 PC 在线不直发)。
const FW_PC_DISPATCH: u16 = 0x0002;

/// OK 双击的判定窗口(ms):第二下按下距上次抬起不超过它就算双击。
/// 与设备按键组件的短按窗口一致(BSP_BTN_SHORT_PRESS_MS = 180)。
const OK_DOUBLE_WINDOW_MS: u32 = 180;

/// 上一次 OK 抬起时的设备时间戳(ButtonEvent.ts_ms);u64::MAX = 本会话还没抬起过。
static OK_LAST_RELEASE_MS: AtomicU64 = AtomicU64::new(u64::MAX);
/// OK 双击删除模式是否进行中(第二下按住期间):这一下不进录音管线。
static OK_DELETE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 纯判定:这次按下是否落在"上次抬起"之后的双击窗口内。
///
/// `last_release_ms` 为 None 表示本连接会话里还没抬起过 —— 第一次按键永远是单击。
/// 时间戳由设备给(`esp_timer_get_time()/1000`,u32 约 49 天回绕),所以差值必须用
/// wrapping_sub;PC 侧到达时刻有链路抖动(连接间隔 15ms 量级),不能用来判 180ms。
fn press_within_double_window(last_release_ms: Option<u32>, press_ms: u32) -> bool {
    match last_release_ms {
        Some(last) => press_ms.wrapping_sub(last) <= OK_DOUBLE_WINDOW_MS,
        None => false,
    }
}

/// 这次 OK 按下是不是"双击的第二下"。
///
/// 为什么自己判、不用设备上报的 BUTTON_DOUBLE:按键组件要等第二下**抬起**、再过
/// short_press_time 才补发 DOUBLE,按住期间它一个事件都不发(状态机停在"等抬起"),
/// "按住连删"无从谈起。按下/抬起是逐次实时上报的,自己按时间戳判更早也更准。
fn ok_press_is_second(press_ms: u32) -> bool {
    let last = OK_LAST_RELEASE_MS.load(Ordering::SeqCst);
    let last = (last != u64::MAX).then_some(last as u32);
    press_within_double_window(last, press_ms)
}

/// 连接失败重试退避:3s 起,失败翻倍,封顶 60s。设备关机时 connect 会长时间
/// 阻塞在 GATT 操作上(设备未上电时 CCCD 写不返回)——这是"待命连接",设备上电
/// 后系统自动完成连接、阻塞解除,因此绝不超时放弃。
///
/// 封顶取 1 分钟:设备上电后最多等一轮退避就该有连接窗口,再长用户看到的就是
/// "设备都开机了,PC 一直未连接"。设备不在时多试几次的代价有限。
const BACKOFF_MIN: Duration = Duration::from_secs(3);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// 链路心跳(Unix 毫秒,0 = 当前无活跃链路)。设备线程每轮 poll 更新一次,
/// 看门狗据此发现"设备线程卡在 WinRT 调用里"——同进程下挂住的是我们自己,
/// 无法像子进程那样丢弃重连,至少要让用户看见真相而不是一直显示"已连接"。
static HEARTBEAT_MS: AtomicU64 = AtomicU64::new(0);
/// 心跳超过此时长没有推进即判定卡死。
const STALL_AFTER: Duration = Duration::from_secs(45);

// ---- 链路活性:有应答才算活着 ----
//
// 判活不能看"写返回 Ok":BLE 写返回成功只说明数据交给了协议栈,不代表设备还在。
// 两种假活链路都会骗过写返回值:
//   ① **设备关机**:写要等到 ATT 超时(或下一次写)才报 Unreachable,期间悬浮窗
//      继续显示"按住说话",日志里要等到"保活探测失败"才发现。
//   ② **半死会话**:设备重启后系统缓存仍认为"已订阅",通知被静默丢弃,而写照样
//      返回 Success —— 只能等用户手动重启软件。
// 两种情况唯一可信的证据都是"设备有没有回帧",所以:链路静默就发探测,探测
// 没人应答就判定失效、断链重连(重连时的 CCCD 双写顺带修好订阅)。
/// 链路静默多久发一次保活探测(有帧往来时不发,不跟音频流抢 ATT)。
const PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// 连续多少次探测一个回帧都没有才判失效:留一次容错,偶发丢一帧不重连。
const PROBES_BEFORE_LOST: u32 = 2;
/// 静默超过此时长(且探测无应答)即判定链路失效。
const RX_TIMEOUT: Duration = Duration::from_secs(15);

/// 该发保活探测了吗:链路静默超过一个探测间隔,且距上次探测也够久了
/// (poll 以 500ms 步进,少了第二个条件会在一次静默里连发数次)。
fn should_probe(quiet: Duration, since_probe: Duration) -> bool {
    quiet >= PROBE_INTERVAL && since_probe >= PROBE_INTERVAL
}

/// 链路是否已判定失效:静默超过 [`RX_TIMEOUT`],且期间发出的探测全部无人应答。
fn link_lost(quiet: Duration, unanswered_probes: u32) -> bool {
    quiet >= RX_TIMEOUT && unanswered_probes >= PROBES_BEFORE_LOST
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `Mutex` 中毒由 `Shared::state_guard` 兜底(见 state.rs)。
fn broadcast(shared: &Shared, app: &tauri::AppHandle) {
    let snapshot = shared.state_guard().clone();
    let _ = app.emit("state", snapshot);
}

// ---- 录音电平(悬浮窗波形) ----

/// 最近一次电平(0.0~1.0,f32 位模式),峰值保持用。
static LEVEL_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// 上次发出电平事件的时间(Unix 毫秒)。
static LEVEL_SENT_MS: AtomicU64 = AtomicU64::new(0);
/// 电平事件的最小间隔:设备 20ms 一块,原样转发就是 50Hz —— 前端波形不需要
/// 这么密,而且 50Hz 的 IPC 对一块透明常显窗口是白烧电。
const LEVEL_MIN_GAP_MS: u64 = 50;
/// 电平映射下限:-60dB 以下算静音(与下面的 RMS/log10 同用 f64)。
const LEVEL_FLOOR_DB: f64 = -60.0;

/// 一块 PCM → 0.0~1.0 的电平。
///
/// 用 dB 而不是线性 RMS:正常说话时 `rms/32768` 只在 0.02~0.10 之间挪动,线性
/// 映射出来的波形基本是贴地的一条线。这里把 -60dB~0dB 拉到 0~1,小音量也能看见
/// 跳动。
fn pcm_level(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = pcm.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    let rms = (sum_sq / pcm.len() as f64).sqrt();
    if rms <= 1.0 {
        return 0.0;
    }
    let db = 20.0 * (rms / 32768.0).log10();
    (((db - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0)) as f32
}

/// 收一块音频时更新电平。
///
/// 电平走**独立事件**而不是并进 `AppState`:悬浮窗要 ~20Hz 刷新,而 `state` 是
/// "整份状态"事件(电池/固件/模型/确认键…),20Hz 广播会让主面板跟着无谓重渲染。
///
/// 峰值保持 + 衰减:块与块之间自然回落到 0 会把波形抖成噪点,人眼读不出音量。
fn push_level(app: &tauri::AppHandle, pcm: &[i16]) {
    let level = pcm_level(pcm);
    let prev = f32::from_bits(LEVEL_BITS.load(Ordering::Relaxed));
    let held = if level > prev { level } else { (prev * 0.7).max(level) };
    LEVEL_BITS.store(held.to_bits(), Ordering::Relaxed);
    let now = now_ms();
    if now.saturating_sub(LEVEL_SENT_MS.load(Ordering::Relaxed)) < LEVEL_MIN_GAP_MS {
        return;
    }
    LEVEL_SENT_MS.store(now, Ordering::Relaxed);
    let _ = app.emit("level", held);
}

/// 电平归零并补发一次:按住开始/结束时清掉上一段的残留,别让波形停在最后一帧。
fn reset_level(app: &tauri::AppHandle) {
    LEVEL_BITS.store(0f32.to_bits(), Ordering::Relaxed);
    LEVEL_SENT_MS.store(0, Ordering::Relaxed);
    let _ = app.emit("level", 0f32);
}

// ---- 语音识别工作线程(单实例 + 有界队列) ----

struct AsrJob {
    pcm: Vec<i16>,
}

static ASR_TX: std::sync::OnceLock<std::sync::mpsc::SyncSender<AsrJob>> =
    std::sync::OnceLock::new();

/// 交给常驻识别线程;队列满说明上一次识别还没结束,丢弃并记日志
/// ——连按 OK 时不能同时跑多个识别,否则文字会乱序。
fn queue_recognition(app: &tauri::AppHandle, shared: &Arc<Shared>, pcm: Vec<i16>) {
    let tx = ASR_TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<AsrJob>(ASR_QUEUE_DEPTH);
        let app = app.clone();
        let shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("vm-asr".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    recognize_and_type(&app, &shared, job.pcm);
                }
            });
        if let Err(e) = spawned {
            diag::log(&format!("识别线程创建失败: {e}"));
        }
        tx
    });
    match tx.try_send(AsrJob { pcm }) {
        Ok(()) => {}
        Err(std::sync::mpsc::TrySendError::Full(_)) => {
            diag::log("上一次识别尚未结束,本次录音已丢弃");
        }
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            diag::log("识别线程已退出,本次录音已丢弃");
        }
    }
}

/// 语音:识别 + 键入(在 vm-asr 工作线程里执行,不阻塞链路循环)。
fn recognize_and_type(app: &tauri::AppHandle, shared: &Arc<Shared>, pcm: Vec<i16>) {
    let Some(_model_task) = shared.try_model_task() else {
        diag::log("模型下载或重置中,本次录音未识别");
        return;
    };
    {
        let mut s = shared.state_guard();
        s.asr = asr_state::RECOGNIZING.into();
    }
    broadcast(shared, app);
    let started = std::time::Instant::now();
    match vibemic_asr::AsrEngine::transcribe(&pcm) {
        Ok(text) => {
            diag::log(&format!(
                "识别 {} 样本({:.2}s) 用时 {:.0}ms → {}",
                pcm.len(),
                pcm.len() as f64 / 16000.0,
                started.elapsed().as_millis(),
                if text.is_empty() { "(空)" } else { &text }
            ));
            let typed = if text.is_empty() {
                true
            } else {
                match dispatch::type_text(&text) {
                    Ok(()) => true,
                    Err(e) => {
                        diag::log(&format!("键入失败: {e}"));
                        false
                    }
                }
            };
            let mut s = shared.state_guard();
            // 键入失败时不留"已识别"假象:悬浮窗显示的是"到底有没有送进焦点窗口"。
            s.last_text = if typed { Some(text).filter(|t| !t.is_empty()) } else { None };
            s.asr = if vibemic_asr::model_ready() {
                asr_state::READY.into()
            } else {
                asr_state::MISSING.into()
            };
        }
        Err(e) => {
            diag::log(&format!("识别失败: {e}"));
            shared.state_guard().asr = format!("error: {e}");
        }
    }
    broadcast(shared, app);
}

fn on_button_event(app: &tauri::AppHandle, shared: &Arc<Shared>, ev: &ButtonEvent) {
    // UP/DOWN:固件 ≥0x0002 且 PC 会话活跃时设备不直发单击,由此处按设置分发;
    // 双击(DOWN=停止、OK=删除)也由本函数负责,见下面的分支。
    if ev.button == button_id::UP && ev.event == button_event::CLICK {
        let (fw, key) = {
            let s = shared.state_guard();
            (s.fw_ver, s.send_key)
        };
        if fw >= FW_PC_DISPATCH {
            diag::log(&format!("UP → 确认({key:?})"));
            if let Err(e) = dispatch::confirm(key) {
                diag::log(&format!("确认键注入失败: {e}"));
            }
        } else {
            diag::log(&format!("固件 0x{fw:04X} < 0x0002,UP 由设备直发"));
        }
        return;
    }
    if ev.button == button_id::DOWN && ev.event == button_event::CLICK {
        let fw = shared.state_guard().fw_ver;
        if fw >= FW_PC_DISPATCH {
            diag::log("DOWN → 取消(Esc)");
            if let Err(e) = dispatch::cancel() {
                diag::log(&format!("取消键注入失败: {e}"));
            }
        } else {
            diag::log(&format!("固件 0x{fw:04X} < 0x0002,DOWN 由设备直发"));
        }
        return;
    }

    // DOWN 双击 = 停止:两记 Esc(DSH 用「双击 Esc」中断当前运行)。
    //
    // 不像单击那样按固件版本门控:0x0004 之前的固件对双击本来就不直发
    // (keymap 没有 DOUBLE 槽位,事件只上报不执行),PC 注入不会双发;0x0004 起
    // 设备在 PC 会话活跃时同样不直发 —— 两条路径都归这里,所以无需版本判断。
    if ev.button == button_id::DOWN && ev.event == button_event::DOUBLE {
        diag::log("DOWN 双击 → 停止(Esc ×2)");
        if let Err(e) = dispatch::cancel_twice() {
            diag::log(&format!("停止键注入失败: {e}"));
        }
        return;
    }

    if ev.button != button_id::OK {
        // UP 的长按由设备 profile 直发;DOWN 长按不是槽位:设备端按住 10 秒是**本地**
        // 蓝牙硬重置(清配对 + 重新广播,与 PC 无关),其余情况两端都不动作;
        // UP 的双击槽位是预留空槽(两端都不注入)。
        return;
    }

    match ev.event {
        button_event::PRESS => {
            // 双击的第二下:这是删除手势,不是说话 —— 直接跳过整条录音管线。
            // 固件 ≥0x0005 时设备自己也会认出这一下:不启动 PTT、麦克风不开(屏上显示
            // DELETE);更早的固件仍会推流,那些帧到这里就被丢掉,不会打成字。
            if ok_press_is_second(ev.ts_ms) {
                OK_DELETE_ACTIVE.store(true, Ordering::SeqCst);
                diag::log("OK 双击 → 删除(按住可连删)");
                if let Err(e) = dispatch::start_delete() {
                    // 线程起不来就只删这一个;标志已置位,松开时照常收尾,不会误当录音。
                    diag::log(&format!("删除线程创建失败: {e}"));
                }
                return;
            }
            // 任何一次正常按下都作废在跑的连删:松开事件万一丢了,不能让它继续删下去。
            dispatch::stop_delete();
            {
                let mut s = shared.state_guard();
                s.ptt = true;
                s.last_text = None;
                // 清掉上一条链路告警:重试时就该重新看这一次的结果。
                if s.asr.starts_with("link:") {
                    s.asr = if vibemic_asr::model_ready() {
                        asr_state::READY.into()
                    } else {
                        asr_state::MISSING.into()
                    };
                }
            }
            audio_buf().clear();
            {
                let mut st = audio_stats();
                *st = AudioStats {
                    press_at: Some(std::time::Instant::now()),
                    ..AudioStats::default()
                };
            }
            COLLECTING.store(true, Ordering::SeqCst);
            reset_level(app); // 波形从 0 起跳,不带上一段的残留
            broadcast(shared, app);
            diag::log("OK press → 开始收音");
        }
        button_event::RELEASE => {
            // 抬起时刻是双击判定的起点:删除路径与正常路径都要记,否则
            // "说完一句马上双击删除"会被判成两次单击。
            OK_LAST_RELEASE_MS.store(ev.ts_ms as u64, Ordering::SeqCst);
            if OK_DELETE_ACTIVE.swap(false, Ordering::SeqCst) {
                dispatch::stop_delete();
                // 删除期间设备仍在推流,这些块在收块时就被丢弃;这里顺手清干净,
                // 别让它们混进下一次录音的统计(会被报成"松开后到达 N 块")。
                audio_buf().clear();
                let _ = std::mem::take(&mut *audio_stats());
                diag::log("OK 双击删除结束");
                return;
            }
            shared.state_guard().ptt = false;
            COLLECTING.store(false, Ordering::SeqCst);
            reset_level(app);
            let summary = log_audio_summary();
            let pcm = std::mem::take(&mut *audio_buf());

            // 音频明显少于按住时长 = 链路把流截断了(曾观察到"恒定 0.34s")。
            // 这种情况下识别只会得到空串或错字,所以明确告知,而不是静默出个空结果
            // 让人以为"识别不出来"。
            if audio_looks_truncated(summary.hold_s, summary.chunks, summary.span_s) {
                let pct = if summary.hold_s > 0.0 {
                    summary.span_s / summary.hold_s * 100.0
                } else {
                    0.0
                };
                diag::log(&format!(
                    "音频被截断:按住 {:.2}s,音频只覆盖到 {:.2}s({pct:.0}%),本次不识别",
                    summary.hold_s, summary.span_s
                ));
                let message = if summary.chunks == 0 {
                    "link: 完全没收到设备音频(检查设备是否已订阅 VM_AUDIO)".to_string()
                } else {
                    format!("link: 录音链路不稳(只收到 {pct:.0}% 音频),请重试")
                };
                shared.state_guard().asr = message;
                broadcast(shared, app);
                return;
            }

            broadcast(shared, app);
            // <300ms 的误触/超短按不出字
            if pcm.len() < 16000 * 3 / 10 {
                if !pcm.is_empty() {
                    diag::log(&format!("录音过短({} 样本),忽略", pcm.len()));
                }
                return;
            }
            diag::log(&format!("OK release → 识别 {} 样本", pcm.len()));
            queue_recognition(app, shared, pcm);
        }
        // PTT 期间设备伴随上报的 LONG/CLICK 等一律忽略
        _ => {}
    }
}

fn audio_buf() -> std::sync::MutexGuard<'static, Vec<i16>> {
    AUDIO_BUF
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---- 音频到达统计 ----
//
// 排查"按住说了话却没字"时必须能区分两种完全不同的原因:
//   * 设备侧停发(notify 失败 / 采集失败 / mbuf 池耗尽)→ 序号连续,但到某个 seq 就没了;
//   * PC 侧丢块或迟到 → 序号出现跳变,或有块在松开之后才到达。
// 只记总样本数看不出这些,所以逐块记序号与到达时刻。

#[derive(Default)]
struct AudioStats {
    chunks: u32,
    first_seq: Option<u16>,
    last_seq: Option<u16>,
    seq_jumps: u32,
    /// 松开之后才到达的音频块(迟到/乱序),最后一次记录的计数。
    late_chunks: u32,
    first_at: Option<std::time::Instant>,
    last_at: Option<std::time::Instant>,
    press_at: Option<std::time::Instant>,
}

static AUDIO_STATS: std::sync::Mutex<AudioStats> = std::sync::Mutex::new(AudioStats {
    chunks: 0,
    first_seq: None,
    last_seq: None,
    seq_jumps: 0,
    late_chunks: 0,
    first_at: None,
    last_at: None,
    press_at: None,
});

fn audio_stats() -> std::sync::MutexGuard<'static, AudioStats> {
    AUDIO_STATS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn note_audio_chunk(seq: Option<u16>, collecting: bool) {
    let now = std::time::Instant::now();
    let mut st = audio_stats();
    if !collecting {
        st.late_chunks += 1;
        return;
    }
    st.chunks += 1;
    if st.first_at.is_none() {
        st.first_at = Some(now);
    }
    st.last_at = Some(now);
    if let Some(seq) = seq {
        match st.last_seq {
            Some(prev) if seq != prev.wrapping_add(1) => st.seq_jumps += 1,
            None => st.first_seq = Some(seq),
            _ => {}
        }
        st.last_seq = Some(seq);
    }
}

/// 一次按住的到达汇总(供截断判定与日志)。
struct AudioSummary {
    hold_s: f32,
    /// 首块→末块的到达时间跨度:这才是"设备还在不在发"的直接证据。
    span_s: f32,
    chunks: u32,
}

/// 这次录音是否被链路截断:按住够久(≥0.6s),但音频的到达跨度明显短于按住时长(<60%),
/// 或者一块都没到。
///
/// 为什么看"跨度"而不是"音频内容时长":设备侧现在会按连接间隔限速(背压),慢链路下
/// 合法的产出就是 30~50ms 一块 —— 内容时长天然短于按住时长,拿它判定会误报。
/// 而"最后一块什么时候到"不受限速影响:链路正常时末块几乎贴着松开时刻。
///
/// 阈值按截断样本定:按住 1.71s 而音频跨度只有 0.34s 属截断,正常录音的跨度
/// 几乎贴着按住时长。
fn audio_looks_truncated(hold_s: f32, chunks: u32, span_s: f32) -> bool {
    hold_s >= 0.6 && (chunks == 0 || span_s < hold_s * 0.6)
}

/// 松开时打一行汇总:块数、序号区间、跳变次数、首末块到达时刻(相对松开)。
fn log_audio_summary() -> AudioSummary {
    let st = std::mem::take(&mut *audio_stats());
    let hold = st
        .press_at
        .map(|t| t.elapsed().as_secs_f32())
        .unwrap_or(0.0);
    let seq = match (st.first_seq, st.last_seq) {
        (Some(first), Some(last)) => format!("seq {first}..{last}"),
        _ => "seq 未知".to_string(),
    };
    let first_off = st.first_at.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
    let last_off = st.last_at.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
    let late = if st.late_chunks > 0 {
        format!(",松开后到达 {} 块", st.late_chunks)
    } else {
        String::new()
    };
    // first_off/last_off 都是"距此刻多久",所以跨度 = first_off - last_off。
    let span = (first_off - last_off).max(0.0);
    diag::log(&format!(
        "收音 {:.2}s / {} 块({:.2}s),{seq},丢块或乱序 {} 次,到达跨度 {:.2}s,首块 -{:.2}s 末块 -{:.2}s{late}",
        hold,
        st.chunks,
        st.chunks as f32 * 0.02,
        st.seq_jumps,
        span,
        first_off,
        last_off
    ));
    AudioSummary {
        hold_s: hold,
        span_s: span,
        chunks: st.chunks,
    }
}

/// 收一块音频:超出单次按住上限就停止采集(等下一次 PRESS 重新开始),
/// 避免 RELEASE 丢失时无界增长。
fn push_audio(app: &tauri::AppHandle, shared: &Arc<Shared>, pcm: &[i16]) {
    let mut buf = audio_buf();
    if buf.len() + pcm.len() > AUDIO_MAX_SAMPLES {
        drop(buf);
        COLLECTING.store(false, Ordering::SeqCst);
        {
            let mut s = shared.state_guard();
            s.ptt = false;
        }
        broadcast(shared, app);
        diag::log("录音超过 90s 上限(RELEASE 可能丢失),已停止采集");
        return;
    }
    buf.extend_from_slice(pcm);
}

fn on_status(app: &tauri::AppHandle, shared: &Shared, frame: &Frame) {
    if let Ok(st) = StatusNotify::parse(&frame.payload) {
        {
            let mut s = shared.state_guard();
            s.battery = st.battery_pct;
            if st.fw_ver > s.fw_ver {
                diag::log(&format!("设备固件版本 0x{:04X}", st.fw_ver));
            }
            s.fw_ver = st.fw_ver;
        }
        broadcast(shared, app);
    }
}

/// 卡死看门狗:只检查"当前有活跃链路"时的心跳推进,空闲/待命连接阶段不误报。
fn spawn_watchdog(app: tauri::AppHandle, shared: Arc<Shared>) {
    let spawned = std::thread::Builder::new()
        .name("device-watchdog".into())
        .spawn(move || {
            let mut warned_at: u64 = 0;
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let hb = HEARTBEAT_MS.load(Ordering::Relaxed);
                if hb == 0 {
                    continue; // 未连接/正在待命连接:没有心跳是正常的
                }
                let now = now_ms();
                if now.saturating_sub(hb) <= STALL_AFTER.as_millis() as u64 {
                    continue;
                }
                if now.saturating_sub(warned_at) < 30_000 {
                    continue; // 同一次卡死别刷屏
                }
                warned_at = now;
                diag::log("设备线程超过 45s 无进展(WinRT 调用疑似挂死);UI 已标记未连接,请重启应用");
                {
                    let mut s = shared.state_guard();
                    s.connected = false;
                    s.ptt = false;
                }
                COLLECTING.store(false, Ordering::SeqCst);
                broadcast(&shared, &app);
            }
        });
    if let Err(e) = spawned {
        diag::log(&format!("看门狗线程创建失败: {e}"));
    }
}

pub fn spawn(app: tauri::AppHandle, shared: Arc<Shared>) {
    spawn_watchdog(app.clone(), shared.clone());
    let spawned = std::thread::Builder::new()
        .name("device-link".into())
        .spawn(move || {
            let mut first_fail = true;
            let mut last_fail_log = std::time::Instant::now();
            let mut backoff = BACKOFF_MIN;
            loop {
                // 同进程链路:WinRT 直接在本进程的设备线程里调用(见文件头注释)。
                let mut link = match WinRtLink::connect() {
                    Ok(l) => l,
                    Err(e) => {
                        HEARTBEAT_MS.store(0, Ordering::Relaxed);
                        // 连不上很常见(设备没电/没开机):首次 + 每 60s 记一条
                        if first_fail || last_fail_log.elapsed() >= Duration::from_secs(60) {
                            diag::log(&format!("连接失败,持续重试中: {e}"));
                            first_fail = false;
                            last_fail_log = std::time::Instant::now();
                        }
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                        continue;
                    }
                };
                backoff = BACKOFF_MIN;
                last_fail_log = std::time::Instant::now();
                {
                    let mut s = shared.state_guard();
                    s.connected = true;
                    s.ptt = false;
                    // 连上了就说明配对没问题:烧录后那块配对提示自动消失,不用用户手动点掉。
                    s.pairing_needed = false;
                }
                broadcast(&shared, &app);
                diag::log(&format!(
                    "已连接 {}",
                    vibemic_protocol::uuids::DEVICE_NAME
                ));

                // 拉一次状态(电量/固件版本)。
                let _ = link.send(&Frame::new(frame_type::CMD_GET_STATUS, Vec::<u8>::new()));

                // 链路保活探测:设备重启后 Windows 可能维持半死会话(设备 CCCD
                // 已清零而系统缓存仍"已订阅",通知静默丢失)。定期 GET_STATUS
                // 既刷新状态,也让失效链路尽快暴露 → 断开重连(重连时的 CCCD
                // 双写修复订阅,顺带取到新固件版本)。
                //
                // 判活只看"有没有收到帧"(见 RX_TIMEOUT 的说明):last_rx 由任何
                // 上行帧刷新(STATUS_NOTIFY / BUTTON_EVENT / 音频块),不靠写返回值。
                let mut last_rx = std::time::Instant::now();
                let mut last_probe = std::time::Instant::now();
                // 本轮静默里"发出去了、但没换来任何帧"的探测次数。
                let mut unanswered: u32 = 0;
                HEARTBEAT_MS.store(now_ms(), Ordering::Relaxed);
                loop {
                    match link.poll(Duration::from_millis(500)) {
                        Ok(Some(frame)) => {
                            last_rx = std::time::Instant::now();
                            unanswered = 0;
                            HEARTBEAT_MS.store(now_ms(), Ordering::Relaxed);
                            match frame.kind {
                                frame_type::BUTTON_EVENT => {
                                    match ButtonEvent::parse(&frame.payload) {
                                        Ok(ev) => on_button_event(&app, &shared, &ev),
                                        Err(e) => diag::log(&format!(
                                            "BUTTON_EVENT 解析失败 {e:?}"
                                        )),
                                    }
                                }
                                vibemic_protocol::audio::AUDIO_CHUNK => {
                                    let collecting = COLLECTING.load(Ordering::SeqCst);
                                    note_audio_chunk(
                                        vibemic_protocol::audio::chunk_seq(&frame.payload),
                                        collecting,
                                    );
                                    if collecting {
                                        if let Some(pcm) =
                                            vibemic_protocol::audio::decode_chunk(&frame.payload)
                                        {
                                            push_level(&app, &pcm);
                                            push_audio(&app, &shared, &pcm);
                                        }
                                    }
                                }
                                frame_type::STATUS_NOTIFY => on_status(&app, &shared, &frame),
                                other => diag::log(&format!("收到未处理帧 type=0x{other:02x}")),
                            }
                        }
                        Ok(None) => {
                            HEARTBEAT_MS.store(now_ms(), Ordering::Relaxed);
                            let quiet = last_rx.elapsed();
                            // 先看是否已经"探测也没人应":设备关机/链路半死都在这里
                            // 收敛,不再依赖某一次写的返回值。
                            if link_lost(quiet, unanswered) {
                                diag::log(&format!(
                                    "链路失效:{:.0}s 内没收到任何设备帧({unanswered} 次保活探测无人应答),重连",
                                    quiet.as_secs_f32()
                                ));
                                break;
                            }
                            // 有帧往来时一次都不发:每次 send 都是一次阻塞的 WinRT
                            // .get(),不该跟音频流抢 ATT(固定间隔发会在录音中途
                            // 也照发)。
                            if should_probe(quiet, last_probe.elapsed()) {
                                if let Err(e) = link.send(&Frame::new(
                                    frame_type::CMD_GET_STATUS,
                                    Vec::<u8>::new(),
                                )) {
                                    diag::log(&format!("保活探测失败,重连: {e}"));
                                    break;
                                }
                                unanswered += 1;
                                last_probe = std::time::Instant::now();
                            }
                        }
                        Err(e) => {
                            diag::log(&format!("链路错误,即将重连: {e}"));
                            break;
                        }
                    }
                }

                HEARTBEAT_MS.store(0, Ordering::Relaxed);
                {
                    let mut s = shared.state_guard();
                    s.connected = false;
                    s.ptt = false;
                    s.last_text = None;
                }
                COLLECTING.store(false, Ordering::SeqCst);
                audio_buf().clear();
                // 连删必须随链路一起停:松开事件丢了也不能继续删用户的文本。
                dispatch::stop_delete();
                OK_DELETE_ACTIVE.store(false, Ordering::SeqCst);
                // 双击判定不能跨连接会话:重连后的第一下永远算单击。
                OK_LAST_RELEASE_MS.store(u64::MAX, Ordering::SeqCst);
                // 断连兜底:OK 若正处于按住态,键入类操作可能卡在半途,借 Drop 全抬
                dispatch::release_all();
                broadcast(&shared, &app);
                // link 在此 Drop:WinRT 注销 ValueChanged 并逐层 Close(winrt.rs)。
                drop(link);
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    if let Err(e) = spawned {
        diag::log(&format!("设备链路线程创建失败: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        audio_looks_truncated, link_lost, pcm_level, press_within_double_window, should_probe,
        PROBES_BEFORE_LOST, PROBE_INTERVAL, RX_TIMEOUT,
    };

    /// OK 双击判定的边界:窗口内算第二下、窗口外算单击、没有上一次抬起则永远是单击。
    ///
    /// 这里锁死的是"删除手势不会被误触发"的前提:说话时连点两下 OK 是常见动作,
    /// 判定必须严进严出 —— 宁可把慢一点的双击当两次单击,也不能把一次单击算成双击。
    #[test]
    fn ok_double_window_boundaries() {
        // 本会话还没抬起过:第一次按下永远是单击
        assert!(!press_within_double_window(None, 0));
        assert!(!press_within_double_window(None, 123_456));

        // 贴着窗口边界:算双击(与设备组件"窗口内即双击"的口径一致)
        assert!(press_within_double_window(Some(1_000), 1_000));
        assert!(press_within_double_window(Some(1_000), 1_180));
        // 超出窗口:算下一次单击
        assert!(!press_within_double_window(Some(1_000), 1_181));
        assert!(!press_within_double_window(Some(1_000), 2_000));

        // 设备时间戳是 u32 毫秒(约 49 天回绕),跨回绕必须是"差 16ms"而不是负数
        assert!(press_within_double_window(Some(u32::MAX - 10), 5));
        assert!(!press_within_double_window(Some(u32::MAX - 10), 500));
    }

    /// 保活判定的三条不变量:有帧往来时不探测、静默后按间隔探测、探测无人应答才判失效。
    ///
    /// 判活只看设备有没有回帧:写返回 Ok 不等于链路还在(设备关机与半死会话都会
    /// 让写成功而通知全丢),只看写返回值会让悬浮窗一直停在"按住说话"。
    #[test]
    fn keepalive_probe_and_liveness_rules() {
        // 刚连上/一直有帧:不探测
        assert!(!should_probe(Duration::ZERO, Duration::ZERO));
        assert!(!should_probe(
            Duration::from_millis(4_999),
            Duration::from_secs(60)
        ));
        // 静默够久,且距上次探测也够久:发探测
        assert!(should_probe(PROBE_INTERVAL, PROBE_INTERVAL));
        // 距上次探测太近就不重发(500ms 轮询会在一次静默里连发数次)
        assert!(!should_probe(
            Duration::from_secs(30),
            PROBE_INTERVAL - Duration::from_millis(500)
        ));

        // 静默超时但只发过 1 次探测:再给一次机会,不判失效
        assert!(!link_lost(RX_TIMEOUT + Duration::from_secs(5), 1));
        // 探测次数够了但静默还没到窗口:继续等
        assert!(!link_lost(
            RX_TIMEOUT - Duration::from_millis(1),
            PROBES_BEFORE_LOST
        ));
        // 静默超时 + 探测无人应答:判失效
        assert!(link_lost(RX_TIMEOUT, PROBES_BEFORE_LOST));
        assert!(link_lost(Duration::from_secs(60), 7));

        // 判失效前必须来得及发出足够次数的探测,否则这条判定永远不会成立
        assert!(RX_TIMEOUT >= PROBE_INTERVAL * PROBES_BEFORE_LOST);
    }

    /// 电平映射:静音必须是 0(否则待机波形抖),满量程到顶,且随音量单调。
    #[test]
    fn pcm_level_maps_loudness_to_unit_range() {
        assert_eq!(pcm_level(&[]), 0.0);
        assert_eq!(pcm_level(&[0i16; 320]), 0.0);
        // ±1LSB 属于底噪,按静音处理(否则 50Hz 的抖动会让波形一直微微抽动)
        assert_eq!(pcm_level(&[1i16; 320]), 0.0);

        let quiet = pcm_level(&[300i16; 320]);
        let talk = pcm_level(&[2000i16; 320]);
        let loud = pcm_level(&[16000i16; 320]);
        assert!(quiet < talk && talk < loud, "音量映射不单调: {quiet} {talk} {loud}");
        assert!(loud <= 1.0 && quiet >= 0.0, "电平越界: {quiet} {loud}");
        // 正常说话要落在看得见的高度区间,而不是贴着 0
        assert!(talk > 0.5, "正常说话电平偏低,波形会像没动: {talk}");
        // 满量程方波 RMS = 峰值 → 顶格
        assert!(pcm_level(&[i16::MAX; 320]) > 0.99);
    }

    /// 阈值必须同时满足两件事:认出截断样本,且不误伤正常录音(含慢链路限速)。
    #[test]
    fn truncation_heuristic_covers_the_observed_failure() {
        // 截断样本:按住 1.71s / 3.36s,音频跨度只有 0.32~0.36s
        assert!(audio_looks_truncated(1.71, 17, 0.34));
        assert!(audio_looks_truncated(3.36, 16, 0.32));
        // 一块都没收到(未订阅 / 订阅失效)
        assert!(audio_looks_truncated(2.00, 0, 0.0));
        // 正常:末块几乎贴着松开
        assert!(!audio_looks_truncated(2.44, 122, 2.40));
        // 正常但尾部还在路上(跨度 90%)
        assert!(!audio_looks_truncated(2.00, 85, 1.80));
        // 慢链路被背压限速:内容只有 2.0s/3.0s,但跨度是满的 → 不能误报
        assert!(!audio_looks_truncated(3.00, 100, 2.96));
        assert!(!audio_looks_truncated(3.00, 60, 2.95));
        // 短按不做截断判定,交给"录音过短"分支
        assert!(!audio_looks_truncated(0.34, 17, 0.32));
        assert!(!audio_looks_truncated(0.20, 0, 0.0));
    }
}
