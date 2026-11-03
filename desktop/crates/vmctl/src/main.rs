//! vmctl —— VibeMic 链路验证 CLI:
//!   vmctl scan                 扫描 BLE + 列串口
//!   vmctl status               连接设备,GET_STATUS,打印状态
//!   vmctl set-active <id>      SET_ACTIVE + 等 RESULT
//!   vmctl watch                持续打印上行帧(按键事件/状态/应答)
//! 全局选项:--serial <PORT>[ --baud N] 走串口通道;--timeout <s> 连接超时
//! (仅对 btleplug 的 BLE 扫描路径生效;Windows 的 WinRT 直连按名连接,无超时)。
//!
//! 提示:设备同时是 OS 的 HID 键盘 —— 先在系统蓝牙设置里配对,再运行本 CLI;
//! 写 VM_CTRL 需要加密链路(设备按 WRITE_ENC 硬性拒绝未加密写)。

use std::process::ExitCode;
use std::time::Duration;

use vibemic_link::{scan_ble, wait_for, BleLink, DeviceLink, LinkError, SerialLink, DEFAULT_BAUD};
#[cfg(target_os = "windows")]
use vibemic_link::WinRtLink;
use vibemic_protocol::frame::{frame_type, Frame};
use vibemic_protocol::payload::{
    bond_slot, button_event, button_id, conn_state, result_code, ButtonEvent, CommandResult,
    StatusNotify,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

struct Options {
    serial: Option<String>,
    baud: u32,
    timeout: Duration,
    command: String,
    args: Vec<String>,
}

fn parse_args(argv: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        serial: None,
        baud: DEFAULT_BAUD,
        timeout: DEFAULT_TIMEOUT,
        command: String::new(),
        args: Vec::new(),
    };
    let mut rest = argv.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            // 设备按名称定位(见 vibemic-protocol::uuids::DEVICE_NAME),不按 MAC:
            // 该参数从未生效,与其静默忽略,不如明确报错。
            "--address" => {
                return Err(format!(
                    "--address is gone: the device is located by name '{}'",
                    vibemic_protocol::uuids::DEVICE_NAME
                ))
            }
            "--serial" => opts.serial = Some(rest.next().ok_or("--serial needs a port")?.clone()),
            "--baud" => {
                opts.baud = rest
                    .next()
                    .ok_or("--baud needs a value")?
                    .parse()
                    .map_err(|_| "invalid baud")?
            }
            "--timeout" => {
                let secs: u64 = rest
                    .next()
                    .ok_or("--timeout needs seconds")?
                    .parse()
                    .map_err(|_| "invalid timeout")?;
                opts.timeout = Duration::from_secs(secs);
            }
            _ if opts.command.is_empty() => opts.command = a.clone(),
            _ => opts.args.push(a.clone()),
        }
    }
    if opts.command.is_empty() {
        return Err("no command".into());
    }
    Ok(opts)
}

fn open_link(opts: &Options) -> Result<Box<dyn DeviceLink>, LinkError> {
    #[cfg(target_os = "windows")]
    if opts.serial.is_none() {
        // Windows 首选 WinRT 直连:已配对设备被 OS 持续连接(HID)而不广播,
        // btleplug 扫描路径够不着;按名枚举 + FromIdAsync 可挂上同一连接。
        println!("[link] winrt direct (paired device)...");
        return Ok(Box::new(WinRtLink::connect()?));
    }
    match &opts.serial {
        Some(port) => {
            println!("[link] serial {port}@{}", opts.baud);
            Ok(Box::new(SerialLink::open(port, opts.baud)?))
        }
        None => {
            println!(
                "[link] BLE, connecting to '{}' ...",
                vibemic_protocol::uuids::DEVICE_NAME
            );
            Ok(Box::new(BleLink::connect(opts.timeout)?))
        }
    }
}

fn fmt_status(f: &Frame) -> Result<String, String> {
    let st = StatusNotify::parse(&f.payload).map_err(|e| format!("{e:?}"))?;
    let conn = match st.conn_state {
        conn_state::ADVERTISING => "advertising",
        conn_state::CONNECTED => "connected",
        other => return Ok(format!("conn_state={other}")),
    };
    Ok(format!(
        "status: fw=0x{:04X} battery={} charging={} conn={conn} profile={} bond={} freeHeap={}",
        st.fw_ver,
        st.battery_pct
            .map(|v| format!("{v}%"))
            .unwrap_or_else(|| "unknown".into()),
        st.charging
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into()),
        st.active_profile_id,
        match st.bond_slot {
            bond_slot::NONE => "none",
            bond_slot::A => "A",
            bond_slot::B => "B",
            _ => "?",
        },
        st.free_heap,
    ))
}

fn fmt_result(f: &Frame) -> Result<String, String> {
    let r = CommandResult::parse(&f.payload).map_err(|e| format!("{e:?}"))?;
    let code = match r.code {
        result_code::OK => "OK",
        result_code::BAD_CRC => "BAD_CRC",
        result_code::BAD_VERSION => "BAD_VERSION",
        result_code::STORAGE => "STORAGE",
        result_code::BUSY => "BUSY",
        result_code::BAD_ARG => "BAD_ARG",
        other => return Ok(format!("RESULT req=0x{:02X} code={other}", r.req_type)),
    };
    Ok(format!("RESULT req=0x{:02X} code={code}", r.req_type))
}

fn fmt_button(f: &Frame) -> Result<String, String> {
    let ev = ButtonEvent::parse(&f.payload).map_err(|e| format!("{e:?}"))?;
    let btn = match ev.button {
        button_id::UP => "UP",
        button_id::DOWN => "DOWN",
        button_id::OK => "OK",
        other => return Ok(format!("button={other}")),
    };
    let name = match ev.event {
        button_event::PRESS => "PRESS",
        button_event::RELEASE => "RELEASE",
        button_event::CLICK => "CLICK",
        button_event::DOUBLE => "DOUBLE",
        button_event::LONG => "LONG",
        other => return Ok(format!("event={other}")),
    };
    Ok(format!("BUTTON {btn} {name} @{}ms", ev.ts_ms))
}

fn fmt_frame(f: &Frame) -> String {
    match f.kind {
        frame_type::STATUS_NOTIFY => fmt_status(f).unwrap_or_else(|e| format!("bad status: {e}")),
        frame_type::RESULT => fmt_result(f).unwrap_or_else(|e| format!("bad result: {e}")),
        frame_type::BUTTON_EVENT => fmt_button(f).unwrap_or_else(|e| format!("bad event: {e}")),
        other => format!("frame type=0x{other:02X} len={}", f.payload.len()),
    }
}

fn cmd_scan(_opts: &Options) -> ExitCode {
    let mut failures = 0;
    match scan_ble(Duration::from_secs(5)) {
        Ok(hits) => {
            if hits.is_empty() {
                println!("BLE: no peripherals seen");
            }
            for h in hits {
                println!(
                    "BLE {} {} rssi={:?}{}",
                    h.id,
                    h.name.as_deref().unwrap_or("(unnamed)"),
                    h.rssi,
                    if h.vibemic { "  <== VibeMic" } else { "" }
                );
            }
        }
        Err(e) => {
            failures += 1;
            eprintln!("BLE scan failed: {e}");
        }
    }
    match SerialLink::list_ports() {
        Ok(ports) => {
            for p in ports {
                println!("SERIAL {p}");
            }
        }
        Err(e) => {
            failures += 1;
            eprintln!("serial ports unavailable: {e}");
        }
    }
    // 两条通道都拿不到结果才算失败:单独 BLE 不可用很常见(未配对/无适配器)。
    if failures == 2 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_status(opts: &Options) -> ExitCode {
    let mut link = match open_link(opts) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    println!("[link] connected via {}", link.name());
    let cmd = Frame::new(frame_type::CMD_GET_STATUS, Vec::<u8>::new());
    if let Err(e) = link.send(&cmd) {
        return fail(e);
    }
    match wait_for(link.as_mut(), Duration::from_secs(3), &|f| {
        f.kind == frame_type::STATUS_NOTIFY
    }) {
        Ok(Some(f)) => {
            println!("{}", fmt_frame(&f));
            ExitCode::SUCCESS
        }
        Ok(None) => {
            println!("no STATUS_NOTIFY within 3s (device connected but silent?)");
            ExitCode::FAILURE
        }
        Err(e) => fail(e),
    }
}

fn cmd_set_active(opts: &Options) -> ExitCode {
    let Some(id) = opts.args.first().and_then(|s| s.parse::<u8>().ok()) else {
        println!("usage: vmctl set-active <profile-id 0..>");
        return ExitCode::FAILURE;
    };
    let mut link = match open_link(opts) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    if let Err(e) = link.send(&Frame::new(frame_type::CMD_SET_ACTIVE, [id])) {
        return fail(e);
    }
    match wait_for(link.as_mut(), Duration::from_secs(3), &|f| {
        f.kind == frame_type::RESULT
    }) {
        Ok(Some(f)) => {
            println!("{}", fmt_frame(&f));
            // 设备用 RESULT.code 表达拒绝(未知 profileId → BAD_ARG);
            // 只看"收到 RESULT"会让脚本把失败当成功。
            match CommandResult::parse(&f.payload) {
                Ok(r) if r.code != result_code::OK => ExitCode::FAILURE,
                _ => ExitCode::SUCCESS,
            }
        }
        Ok(None) => {
            println!("no RESULT within 3s");
            ExitCode::FAILURE
        }
        Err(e) => fail(e),
    }
}

fn cmd_watch(opts: &Options) -> ExitCode {
    let mut link = match open_link(opts) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    println!(
        "[watch] {} — press buttons on the device; Ctrl-C to stop",
        link.name()
    );
    loop {
        match link.poll(Duration::from_secs(30)) {
            Ok(Some(f)) => println!("{}", fmt_frame(&f)),
            Ok(None) => println!("[watch] ... idle"),
            Err(e) => return fail(e),
        }
    }
}

/// 录音验证:连接后等待设备端按住 OK(设备采集串流),OK 松开或超时后
/// 把重组解码的 PCM 落盘为 WAV。语音链路的验收工具。
fn cmd_record(opts: &Options) -> ExitCode {
    let path = opts
        .args
        .first()
        .cloned()
        .unwrap_or_else(|| "vibemic.wav".into());
    let mut link = match open_link(opts) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    println!(
        "[record] {} — 按住设备 OK 说话,松开后写 {};Ctrl-C 放弃",
        link.name(),
        path
    );
    let mut pcm: Vec<i16> = Vec::new();
    let mut chunks = 0u32;
    let mut gaps = 0u32;
    let mut expect_seq: Option<u16> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        if std::time::Instant::now() > deadline {
            println!("[record] 120s 超时,按现有数据落盘");
            break;
        }
        match link.poll(Duration::from_millis(500)) {
            Ok(Some(f)) => {
                if f.kind == vibemic_protocol::audio::AUDIO_CHUNK {
                    if let Some(seq) = vibemic_protocol::audio::chunk_seq(&f.payload) {
                        if let Some(want) = expect_seq {
                            if seq != want {
                                gaps += 1;
                                println!("[record] seq 跳变 {want} → {seq}");
                            }
                        }
                        expect_seq = Some(seq.wrapping_add(1));
                    }
                    match vibemic_protocol::audio::decode_chunk(&f.payload) {
                        Some(samples) => {
                            pcm.extend_from_slice(&samples);
                            chunks += 1;
                        }
                        None => println!("[record] 块过短,丢弃 ({}B)", f.payload.len()),
                    }
                } else if f.kind == frame_type::BUTTON_EVENT {
                    println!("[record] {}", fmt_frame(&f));
                    if let Ok(ev) = ButtonEvent::parse(&f.payload) {
                        // OK 松开 = 设备侧已停止串流
                        if ev.button == button_id::OK && ev.event == button_event::RELEASE
                            && !pcm.is_empty()
                        {
                            break;
                        }
                    }
                } else {
                    println!("{}", fmt_frame(&f));
                }
            }
            Ok(None) => {}
            Err(e) => {
                if pcm.is_empty() {
                    return fail(e);
                }
                println!("[record] 链路断开({e}),按现有数据落盘");
                break;
            }
        }
    }
    if pcm.is_empty() {
        println!("error: 未收到任何音频块(固件是否已含 0x7a05?OK 是否在 PC 会话活跃时按下?)");
        return ExitCode::FAILURE;
    }
    let wav = vibemic_protocol::audio::pcm_to_wav(&pcm, 16000);
    if let Err(e) = std::fs::write(&path, &wav) {
        println!("error: 写 {path}: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "[record] {} 块({gaps} 跳变) / {} 样本 / {:.2}s → {}",
        chunks,
        pcm.len(),
        pcm.len() as f64 / 16000.0,
        path
    );
    ExitCode::SUCCESS
}

fn fail(e: LinkError) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&argv) {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "vmctl: {e}\n\nusage: vmctl [--serial PORT [--baud N]] [--timeout S] <scan|status|set-active <id>|watch|record [out.wav]>"
            );
            return ExitCode::FAILURE;
        }
    };
    match opts.command.as_str() {
        "scan" => cmd_scan(&opts),
        "status" => cmd_status(&opts),
        "set-active" => cmd_set_active(&opts),
        "watch" => cmd_watch(&opts),
        "record" => cmd_record(&opts),
        other => {
            eprintln!("vmctl: unknown command '{other}'");
            ExitCode::FAILURE
        }
    }
}
