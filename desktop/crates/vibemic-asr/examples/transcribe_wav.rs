//! 离线验证:WAV(16kHz mono s16)→ SenseVoice 识别文本。
//! 用法: cargo run -p vibemic-asr --example transcribe_wav -- <file.wav>
use std::io::Read;

/// 解析 WAV:要求是 16bit 单声道 PCM,返回 PCM 采样。
/// 按 chunk 遍历解析,不跳固定头 —— 短文件/其它 WAV 形状不会 panic 或静默错解。
fn read_wav(path: &str) -> Vec<i16> {
    let mut f = std::fs::File::open(path).expect("open wav");
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).expect("read wav");

    assert!(buf.len() >= 44, "WAV 太短: {} 字节", buf.len());
    assert_eq!(&buf[0..4], b"RIFF", "不是 RIFF 文件");
    assert_eq!(&buf[8..12], b"WAVE", "不是 WAVE 文件");
    // 从 12 字节处遍历 chunk,找到 fmt(核对格式)与 data(取载荷)。
    let mut offset = 12;
    let mut format_ok = false;
    let mut pcm_bytes: Option<&[u8]> = None;
    while offset + 8 <= buf.len() {
        let id = &buf[offset..offset + 4];
        let size = u32::from_le_bytes([
            buf[offset + 4],
            buf[offset + 5],
            buf[offset + 6],
            buf[offset + 7],
        ]) as usize;
        let body = offset + 8;
        if body + size > buf.len() {
            break; // 截断的 chunk:忽略剩余部分
        }
        if id == b"fmt " && size >= 16 {
            let channels = u16::from_le_bytes([buf[body + 2], buf[body + 3]]);
            let rate = u32::from_le_bytes([
                buf[body + 4],
                buf[body + 5],
                buf[body + 6],
                buf[body + 7],
            ]);
            let bits = u16::from_le_bytes([buf[body + 14], buf[body + 15]]);
            format_ok = channels == 1 && bits == 16;
            println!("[wav] {rate} Hz / {channels} ch / {bits} bit");
        } else if id == b"data" {
            pcm_bytes = Some(&buf[body..body + size]);
            break;
        }
        offset = body + size + (size & 1); // chunk 按偶数字节对齐
    }
    assert!(format_ok, "只支持 16bit 单声道 PCM WAV");
    let pcm_bytes = pcm_bytes.expect("WAV 里没有 data chunk");
    let mut pcm = Vec::with_capacity(pcm_bytes.len() / 2);
    for pair in pcm_bytes.chunks(2) {
        if pair.len() < 2 {
            break;
        }
        pcm.push(i16::from_le_bytes([pair[0], pair[1]]));
    }
    pcm
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: transcribe_wav <file.wav>");
    let pcm = read_wav(&path);
    let t = std::time::Instant::now();
    match vibemic_asr::AsrEngine::transcribe(&pcm) {
        Ok(text) => println!(
            "[asr] {} 样本({:.2}s) 用时 {:.0}ms\n[text] {text}",
            pcm.len(),
            pcm.len() as f64 / 16000.0,
            t.elapsed().as_millis()
        ),
        Err(e) => {
            eprintln!("[asr] 失败: {e}");
            std::process::exit(1);
        }
    }
}
