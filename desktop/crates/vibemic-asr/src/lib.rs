//! VibeMic 本地语音识别:sherpa-onnx + SenseVoice-Small(int8)。
//!
//! 模型管理:文件放 `%APPDATA%/vibemic/models/sense-voice/`(model.int8.onnx +
//! tokens.txt),缺失时由 `download_model` 从 ModelScope 国内镜像拉取(约 160MB)。
//! 识别开 `use_itn`:输出自带标点 + 数字正则化("一百二十三"→123),无需外挂
//! 标点模型。
//!
//! 下载的健壮性约定:
//! - ureq 默认只设连接超时,读超时是 None → 必须显式设,否则服务端停顿会让
//!   下载线程永久挂住,而 UI 在下载期间拒绝重入(状态钉在 downloading);
//! - content-length 是权威值,下完必须相符,否则删掉半截文件报错 ——
//!   "读到 EOF 就当成功"会把截断文件 rename 成正式模型;
//! - `model_ready()` 同时校验文件体积,半截模型不会被视为就绪(否则重试下载
//!   会因为文件存在而立刻返回 Ok,形成无法自愈的死胡同);
//! - `reset_model()` 给"识别引擎加载失败"提供强制重下的出口。
//!
//! 换模型只动本 crate:AsrEngine 持有 sherpa-onnx OfflineRecognizer,
//! 16kHz s16 PCM 进、带标点文本出。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig,
};

/// 模型文件名(下载目标名)。
const MODEL_FILE: &str = "model.int8.onnx";
const TOKENS_FILE: &str = "tokens.txt";

/// ModelScope 国内镜像(与官方导出同源)。
const MODEL_URL: &str = "https://www.modelscope.cn/models/pengzhendong/sherpa-onnx-sense-voice-zh-en-ja-ko-yue/resolve/master/";
/// 进度条回退总长:仅在响应没有 content-length 时使用,不参与校验。
/// 模型约 160MB,这里留余量。
const MODEL_SIZE_HINT: u64 = 170 * 1024 * 1024;
/// 模型文件的最小合理体积:小于它必定是截断/损坏(见 `model_ready`)。
const MODEL_MIN_BYTES: u64 = 20 * 1024 * 1024;
/// tokens.txt 的最小合理体积(实际约 300KB)。
const TOKENS_MIN_BYTES: u64 = 1_000;
/// 连接/读/写超时。没有读超时 = 一个卡住的连接就能让本次运行永远下不完。
const NET_TIMEOUT: Duration = Duration::from_secs(30);

/// 模型目录(%APPDATA%/vibemic/models/sense-voice)。
pub fn model_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vibemic")
        .join("models")
        .join("sense-voice")
}

fn file_at_least(path: &Path, min_bytes: u64) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() >= min_bytes)
        .unwrap_or(false)
}

/// 模型是否就绪:两个文件都在,且体积达到最小合理值(半截文件不算就绪)。
pub fn model_ready() -> bool {
    let dir = model_dir();
    file_at_least(&dir.join(MODEL_FILE), MODEL_MIN_BYTES)
        && file_at_least(&dir.join(TOKENS_FILE), TOKENS_MIN_BYTES)
}

/// 清掉已下载的模型与遗留的 `.part`:识别引擎加载失败时的"重新下载"入口
/// (文件存在但内容损坏时,普通下载路径会直接判定就绪并返回 Ok)。
pub fn reset_model() -> Result<(), String> {
    let dir = model_dir();
    for name in [MODEL_FILE, TOKENS_FILE] {
        let path = dir.join(name);
        if path.is_file() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("删除 {} 失败: {e}", path.display()))?;
        }
    }
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("part") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

/// 下载模型(阻塞;progress 回调收 已下载字节数)。已就绪时直接成功。
pub fn download_model(progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    if model_ready() {
        return Ok(());
    }
    let dir = model_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {e}"))?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(NET_TIMEOUT)
        .timeout_read(NET_TIMEOUT)
        .timeout_write(NET_TIMEOUT)
        .build();
    // 缺哪个下哪个:只坏了一个文件时不必把 160MB 重下一遍。
    for (name, hint, min) in [
        (MODEL_FILE, MODEL_SIZE_HINT, MODEL_MIN_BYTES),
        (TOKENS_FILE, 4 * 1024, TOKENS_MIN_BYTES),
    ] {
        let dest = dir.join(name);
        if file_at_least(&dest, min) {
            continue;
        }
        let url = format!("{MODEL_URL}{name}");
        download_file(&agent, &url, &dest, hint, progress)?;
    }
    if !model_ready() {
        return Err("下载结束但模型文件仍不完整".into());
    }
    Ok(())
}

fn download_file(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    hint: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let tmp = dest.with_extension("part");
    match download_to(agent, url, &tmp, hint, progress) {
        // 先落临时文件再 rename:中断不会留下一个"看起来完整"的正式文件。
        Ok(()) => std::fs::rename(&tmp, dest).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("落盘失败: {e}")
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp); // 半截文件不留在盘上
            Err(e)
        }
    }
}

fn download_to(
    agent: &ureq::Agent,
    url: &str,
    tmp: &Path,
    hint: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let resp = agent.get(url).call().map_err(|e| format!("请求失败: {e}"))?;
    // content-length 是权威值:下载字节数必须与它相符。
    let declared = resp
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok());
    let total = declared.unwrap_or(hint);
    let mut reader = resp.into_reader();
    let mut writer = std::io::BufWriter::new(
        std::fs::File::create(tmp).map_err(|e| format!("创建临时文件失败: {e}"))?,
    );
    let mut buf = [0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("下载中断: {e}"))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| format!("写盘失败: {e}"))?;
        done += n as u64;
        progress(done, total);
    }
    writer.flush().map_err(|e| format!("写盘失败: {e}"))?;
    drop(writer);
    if let Some(expected) = declared {
        if done != expected {
            return Err(format!(
                "下载不完整: 收到 {done} 字节,服务器声明 {expected} 字节"
            ));
        }
    } else if done < 1024 * 1024 {
        return Err(format!("下载内容过小({done} 字节),疑似失败"));
    }
    Ok(())
}

/// 识别引擎(进程级常驻;线程安全:sherpa-onnx C API 内部持锁)。
pub struct AsrEngine {
    rec: OfflineRecognizer,
}

static ENGINE: Mutex<Option<AsrEngine>> = Mutex::new(None);

impl AsrEngine {
    fn new() -> Result<Self, String> {
        if !model_ready() {
            return Err("模型未下载或文件不完整".into());
        }
        let dir = model_dir();
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                sense_voice: OfflineSenseVoiceModelConfig {
                    model: Some(dir.join(MODEL_FILE).to_string_lossy().into_owned()),
                    language: Some("zh".into()),
                    use_itn: true, // 输出带标点 + ITN(见模块注释)
                },
                tokens: Some(dir.join(TOKENS_FILE).to_string_lossy().into_owned()),
                num_threads: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let rec = OfflineRecognizer::create(&config)
            .ok_or_else(|| "加载模型失败(路径错误或模型损坏)".to_string())?;
        Ok(AsrEngine { rec })
    }

    /// 识别一段 16kHz s16 PCM → 文本(阻塞,数十到几百毫秒)。
    /// 模型加载失败时丢弃已缓存的引擎,下次调用会重新尝试加载
    /// (否则"重下模型"之后进程内仍拿着加载失败的旧状态)。
    pub fn transcribe(pcm: &[i16]) -> Result<String, String> {
        if !model_ready() {
            return Err("模型未下载或文件不完整".into());
        }
        let mut guard = ENGINE.lock().map_err(|e| e.to_string())?;
        if guard.is_none() {
            match AsrEngine::new() {
                Ok(engine) => *guard = Some(engine),
                Err(e) => {
                    *guard = None;
                    return Err(e);
                }
            }
        }
        let engine = guard.as_ref().unwrap();
        let stream = engine.rec.create_stream();
        // sherpa-onnx C API 收 f32;设备侧 PCM 为 s16。
        let samples: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        stream.accept_waveform(16000, &samples);
        engine.rec.decode(&stream);
        let text = stream
            .get_result()
            .map(|r| r.text)
            .unwrap_or_default();
        Ok(normalize_transcript(&text))
    }
}

/// 识别结果收尾:去掉首尾空白,并**去掉末尾的句读**。
///
/// SenseVoice 会给中文加标点,末尾几乎总跟一个"。" —— 直接键入焦点窗口时用户看到
/// 的就是"编译好了。"里那个多余的句号。这里只动**末尾**的句读,句中标点原样保留
/// (否则"第一,第二"会被改坏)。
///
/// 末尾若是纯标点(模型对静音/噪声的输出就是"。"这类),归一化后是空串,调用方
/// 什么都不键入 —— 比往光标处打一个句号好。
pub fn normalize_transcript(raw: &str) -> String {
    /// 末尾要剥掉的句读(中英文)。不含引号/括号:那些成对出现,剥掉反而残缺。
    const TRAILING: [char; 16] = [
        '。', '．', '.', '！', '!', '？', '?', '；', ';', '：', ':', '，', ',', '、', '…', '~',
    ];
    raw.trim_end_matches(|c: char| c.is_whitespace() || TRAILING.contains(&c))
        .trim_start()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::normalize_transcript;

    #[test]
    fn trailing_punctuation_is_dropped() {
        // 末尾那个句号要被去掉
        assert_eq!(normalize_transcript("编译好了。"), "编译好了");
        assert_eq!(normalize_transcript("你好，今天星期几？"), "你好，今天星期几");
        assert_eq!(normalize_transcript("Hello, world."), "Hello, world");
        assert_eq!(normalize_transcript("你说什么？！"), "你说什么"); // 连续句读一起去掉
        assert_eq!(normalize_transcript("好。  "), "好");
        // 纯标点 = 静音/噪声的空结果,归一化后应为什么都不键入
        assert_eq!(normalize_transcript("。"), "");
        assert_eq!(normalize_transcript("  "), "");
        assert_eq!(normalize_transcript(""), "");
        // 句中标点与正常尾字符都不能动
        assert_eq!(normalize_transcript("第一，第二"), "第一，第二");
        assert_eq!(normalize_transcript("版本 0.2"), "版本 0.2");
        assert_eq!(normalize_transcript("  前面有空格"), "前面有空格");
    }
}
