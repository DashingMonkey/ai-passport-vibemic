//! 设备固件烧录:把 espflash(esp-rs 的 esptool 实现)嵌进桌面端后端。
//!
//! 只走官方交付方式 —— 合并镜像 `FoloToy-AI-Passport-full.bin` 从 `0x0` 写入,
//! 写前验证 C3 镜像完整性、分区表及应用边界,并显示 SHA-256 摘要,并把进度与日志
//! 回吐给前端。与官方网页烧录工具(esptool-js + WebSerial)相比,产物、偏移、
//! 校验口径一致,但不需要浏览器,也不需要用户自己装 Python。
//!
//! 为什么用 espflash 而不是 `python -m esptool`:仓库 `environment-setup.md` 的
//! 官方命令确实是 esptool,而 espflash 是 esptool 在 Rust 生态里的等价移植
//! (其 README 自述 "based on esptool.py"),偏移与镜像产物同源;桌面端要交付
//! 单个 exe,不能把 Python 运行时装进包里。需要严格同一程序时,可另加一条
//! "调用系统 esptool"的备用路径。

use std::io::Read;
use std::path::{Path, PathBuf};

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::target::{Chip, ProgressCallbacks};
use serde::Serialize;
use serialport::{FlowControl, SerialPortType};
use sha2::{Digest, Sha256};

/// 官方交付镜像名(`device/tools/archive_firmware.py` 的产物)。
pub const MERGED_IMAGE: &str = "FoloToy-AI-Passport-full.bin";
/// 合并镜像必须写到 0x0:它已含 bootloader + 分区表 + 应用。
pub const MERGED_IMAGE_ADDR: u32 = 0x0000;
/// 官方文档给的烧录波特率(`python -m esptool … -b 460800`)。
pub const DEFAULT_BAUD: u32 = 460_800;
/// 连接阶段固定 115200(esptool/espflash 都如此),连上后再切到上面那个。
const CONNECT_BAUD: u32 = 115_200;
/// ESP 镜像魔数:合并镜像的首字节。
const ESP_IMAGE_MAGIC: u8 = 0xE9;
/// Espressif 的 USB VID,与 ESP32-C3 内置 USB-Serial/JTAG 的 PID。
/// 命中它说明设备自带下载模式、不用手按 BOOT;espflash 也据此选复位序列,
/// 所以枚举端口时必须把真实 pid 交给它,不能像 CLI 那样在非 USB 口上填 0。
const ESPRESSIF_VID: u16 = 0x303A;
const USB_SERIAL_JTAG_PID: u16 = 0x1001;

/// 待烧镜像的自检结果(前端在按下"写入"之前先看这个)。
#[derive(Debug, Clone, Serialize)]
pub struct FirmwareInfo {
    /// 实际使用的镜像路径(配置留空时 = 在 exe 同目录里按 [`candidates_in`] 找到的那条)。
    pub path: String,
    /// exe 所在目录:前端提示"默认去哪儿找"用。
    pub exe_dir: String,
    /// "选择固件"对话框里要预选哪一个文件。必须真实存在才算数:
    /// `None` = 软件目录里没有镜像,前端干脆不传 `defaultPath`,
    /// 由用户自己选目录挑文件。
    pub default_path: Option<String>,
    pub exists: bool,
    pub size: u64,
    /// 小写十六进制;读不到文件时为空串。
    pub sha256: String,
    /// 首字节是否为 0xE9。
    pub magic_ok: bool,
    /// 不能烧的原因;None = 可以烧。
    pub error: Option<String>,
}

/// 可选串口(前端下拉框)。
#[derive(Debug, Clone, Serialize)]
pub struct PortEntry {
    pub name: String,
    pub label: String,
    /// USB 串口且 pid 命中 ESP32 USB-Serial/JTAG。
    pub usb_jtag: bool,
}

/// 烧录过程中的事件,由调用方决定怎么送到前端(Tauri 事件)。
pub enum FlashEvent {
    Progress { done: usize, total: usize },
    Verifying,
    Written { skipped: bool },
    Log(String),
}

/// 一次烧录的参数。
pub struct FlashRequest<'a> {
    pub port: &'a str,
    pub path: &'a Path,
    pub baud: u32,
    pub verify: bool,
    pub erase: bool,
    pub chip: Chip,
}

/// exe 所在目录(取不到时退回当前目录)。
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 默认找镜像的两个位置,按优先级:
/// 1. `<base>/firmware/<MERGED_IMAGE>` —— 交付包的摆法(打包脚本把镜像放进子目录);
/// 2. `<base>/<MERGED_IMAGE>` —— "PC 软件和设备固件丢同一个文件夹"的散装摆法。
///
/// `base` 是参数而不是直接取 [`exe_dir`],是为了能用临时目录测出"存在优先"的挑选逻辑。
pub fn candidates_in(base: &Path) -> Vec<PathBuf> {
    vec![
        base.join("firmware").join(MERGED_IMAGE),
        base.join(MERGED_IMAGE),
    ]
}

/// 在候选里挑默认镜像:存在的按顺序优先;一条都不存在时返回第一条,
/// 让上层提示里能说出"我默认找的是哪儿"。
pub fn pick_default(base: &Path) -> PathBuf {
    let mut candidates = candidates_in(base);
    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .unwrap_or_else(|| candidates.remove(0))
}

/// 配置留空时,镜像取 exe 同目录里 [`candidates_in`] 找到的那条
/// ——正是"PC 软件和设备固件放同一个文件夹"的约定。
///
/// 配置非空时严格只用它:用户手选过具体文件,就算那个文件没了也不该悄悄换成
/// 别的镜像去烧(那是"我明明选了 A,你却烧了 B")。
pub fn resolve_firmware_path(configured: &str) -> PathBuf {
    resolve_in(&exe_dir(), configured)
}

/// [`resolve_firmware_path`] 的实现体:`base` 可注入,便于在临时目录里测挑选逻辑。
fn resolve_in(base: &Path, configured: &str) -> PathBuf {
    let trimmed = configured.trim();
    if !trimmed.is_empty() {
        return PathBuf::from(trimmed);
    }
    pick_default(base)
}

/// "选择固件"对话框该预选哪个文件 —— 只有真实存在的镜像才算数。
///
/// 顺序:当前生效的那条存在就用它;否则在软件目录里按候选顺序找;都没有 = `None`,
/// 前端于是**不传** `defaultPath`。为什么不给个"名义上的默认路径":Windows 的
/// 文件对话框会把不存在的默认路径里的文件名预填进"文件名"框,用户点确定只会
/// 得到"找不到文件",还不如让他自己从任意目录挑。
pub fn dialog_default(path: &Path) -> Option<PathBuf> {
    dialog_default_in(&exe_dir(), path)
}

fn dialog_default_in(base: &Path, path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    candidates_in(base).into_iter().find(|p| p.is_file())
}

/// 路径是否属于"软件目录里的默认候选"(用于把"找不到"讲成人话)。
fn is_candidate(path: &Path) -> bool {
    candidates_in(&exe_dir()).iter().any(|p| p == path)
}

/// 读取并验证完整合并镜像;SHA-256 摘要同时提供给前端比对。
pub fn inspect(path: &Path) -> FirmwareInfo {
    let mut info = FirmwareInfo {
        path: path.display().to_string(),
        exe_dir: exe_dir().display().to_string(),
        default_path: dialog_default(path).map(|p| p.display().to_string()),
        exists: false,
        size: 0,
        sha256: String::new(),
        magic_ok: false,
        error: None,
    };

    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            info.error = Some(if is_candidate(path) {
                format!(
                    "软件所在目录({})里没找到 {MERGED_IMAGE},请点「选择 .bin…」自己挑一个",
                    exe_dir().display()
                )
            } else {
                format!("读不到固件文件: {e}")
            });
            return info;
        }
    };
    info.exists = true;
    info.size = meta.len();
    if info.size == 0 {
        info.error = Some("固件文件是空的".to_string());
        return info;
    }

    let data = match read_image(path) {
        Ok(data) => data,
        Err(e) => {
            info.error = Some(e);
            return info;
        }
    };
    info.size = data.len() as u64;
    info.sha256 = format!("{:x}", Sha256::digest(&data));
    info.magic_ok = data.first() == Some(&ESP_IMAGE_MAGIC);
    info.error = crate::firmware_image::validate(&data).err();
    info
}

// Bound memory before parsing an untrusted file; one extra byte detects oversize.
fn read_image(path: &Path) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| format!("打不开固件文件: {e}"))?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("读固件失败: {e}"))?;
    if data.len() > 8 * 1024 * 1024 {
        return Err("镜像超过设备 8 MB Flash 容量".into());
    }
    Ok(data)
}

/// 枚举串口。只读,不打开任何端口。
pub fn list_ports() -> Vec<PortEntry> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };
    ports
        .into_iter()
        .map(|port| {
            let (label, usb_jtag) = match &port.port_type {
                SerialPortType::UsbPort(usb) => {
                    let product = usb.product.clone().unwrap_or_default();
                    let label = if product.trim().is_empty() {
                        port.port_name.clone()
                    } else {
                        format!("{} ({})", port.port_name, product)
                    };
                    (
                        label,
                        usb.vid == ESPRESSIF_VID && usb.pid == USB_SERIAL_JTAG_PID,
                    )
                }
                _ => (port.port_name.clone(), false),
            };
            PortEntry {
                name: port.port_name,
                label,
                usb_jtag,
            }
        })
        .collect()
}

/// 名称 → 芯片。目前只认官方交付的 C3(留函数是为了将来多机型时只改这一处)。
pub fn chip_from_name(name: &str) -> Option<Chip> {
    match name.trim().to_ascii_lowercase().as_str() {
        "esp32c3" | "esp32-c3" | "c3" | "" => Some(Chip::Esp32c3),
        _ => None,
    }
}

/// 找到端口并带上真实的 USB 信息(espflash 靠 pid 判断要不要走 USB-JTAG 复位)。
fn usb_port_info(port_name: &str) -> Result<serialport::UsbPortInfo, String> {
    let ports = serialport::available_ports().map_err(|e| format!("枚举串口失败: {e}"))?;
    for port in ports {
        if port.port_name != port_name {
            continue;
        }
        return match port.port_type {
            SerialPortType::UsbPort(info) => Ok(info),
            _ => Err(format!("{port_name} 不是 USB 串口,无法烧录")),
        };
    }
    Err(format!("没找到串口 {port_name}"))
}

/// 执行烧录:校验 → 打开串口 → 连接(含复位进下载模式)→ 从 0x0 写入 → 校验。
///
/// 阻塞调用,调用方负责放到后台线程;进度与日志经 `sink` 回吐。
pub fn flash<F: FnMut(FlashEvent)>(req: &FlashRequest<'_>, sink: &mut F) -> Result<(), String> {
    let data = read_image(req.path)?;
    crate::firmware_image::validate(&data)?;
    sink(FlashEvent::Log(format!(
        "镜像 {} 字节,目标 {} 的 0x{:X}",
        data.len(),
        req.chip,
        MERGED_IMAGE_ADDR
    )));

    // 端口信息必须在打开之前拿到:连接对象一经构造就带着它。
    let usb = usb_port_info(req.port)?;
    if usb.vid == ESPRESSIF_VID && usb.pid == USB_SERIAL_JTAG_PID {
        sink(FlashEvent::Log(
            "设备自带 USB-Serial/JTAG:自动复位进下载模式,无需按 BOOT".to_string(),
        ));
    }

    let serial_port = serialport::new(req.port, CONNECT_BAUD)
        .flow_control(FlowControl::None)
        .open_native()
        .map_err(|e| format!("打开串口 {} 失败: {e}", req.port))?;

    let connection = Connection::new(
        serial_port,
        usb,
        ResetAfterOperation::HardReset,
        ResetBeforeOperation::DefaultReset,
        req.baud,
    );

    sink(FlashEvent::Log(format!(
        "连接设备(先 {CONNECT_BAUD} 波特,连上切 {} 波特)…",
        req.baud
    )));
    // use_stub: 走 flasher stub(快得多);verify: 写完回读比对;skip: 内容一致就跳过。
    let mut flasher = Flasher::connect(connection, true, req.verify, true, Some(req.chip), Some(req.baud))
        .map_err(|e| format!("连接设备失败: {e}"))?;
    sink(FlashEvent::Log(format!("已连接:{}", flasher.chip())));

    if req.erase {
        sink(FlashEvent::Log("整片擦除中…".to_string()));
        flasher
            .erase_flash()
            .map_err(|e| format!("擦除失败: {e}"))?;
    }

    let mut progress = EventProgress::new(sink);
    flasher
        .write_bin_to_flash(MERGED_IMAGE_ADDR, &data, &mut progress)
        .map_err(|e| format!("写入失败: {e}"))?;
    Ok(())
}

/// 把 espflash 的进度回调转成 [`FlashEvent`]。
struct EventProgress<'a, F: FnMut(FlashEvent)> {
    sink: &'a mut F,
    total: usize,
    finished: bool,
}

impl<'a, F: FnMut(FlashEvent)> EventProgress<'a, F> {
    fn new(sink: &'a mut F) -> Self {
        EventProgress {
            sink,
            total: 0,
            finished: false,
        }
    }
}

impl<F: FnMut(FlashEvent)> ProgressCallbacks for EventProgress<'_, F> {
    fn init(&mut self, addr: u32, total: usize) {
        self.total = total;
        self.finished = false;
        (self.sink)(FlashEvent::Log(format!(
            "开始写入 0x{addr:08X} 起 {total} 字节"
        )));
        (self.sink)(FlashEvent::Progress { done: 0, total });
    }

    fn update(&mut self, current: usize) {
        (self.sink)(FlashEvent::Progress {
            done: current,
            total: self.total,
        });
    }

    fn verifying(&mut self) {
        (self.sink)(FlashEvent::Verifying);
    }

    fn finish(&mut self, skipped: bool) {
        self.finished = true;
        (self.sink)(FlashEvent::Written { skipped });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用临时目录(每个用例一个,避免并行跑时互相踩)。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vibemic-flash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 路径解析的两条分支:配置为空 → 软件目录里的候选;配置非空 → 原样使用(严格)。
    #[test]
    fn firmware_path_resolution() {
        let base = temp_dir("resolve");

        // 空配置(含纯空白)= 回到候选,exe 那条真实路径也必须落在候选集合里。
        assert_eq!(resolve_in(&base, "   "), pick_default(&base));
        let auto = resolve_firmware_path("   ");
        assert_eq!(auto.file_name().unwrap(), MERGED_IMAGE);
        assert!(
            candidates_in(&exe_dir()).contains(&auto),
            "空配置解析出的 {auto:?} 不在软件目录的候选里"
        );

        // 手选过的路径原样使用 —— 哪怕它已经不存在,也不许悄悄换成别的镜像
        // (否则就成了"我明明选了 A,你却烧了 B")。
        for picked in [r"D:\some\dir\my.bin", r"D:\nope\gone.bin"] {
            assert_eq!(resolve_firmware_path(picked), PathBuf::from(picked));
            assert_eq!(resolve_in(&base, picked), PathBuf::from(picked));
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 默认镜像的挑选:子目录与同目录两种摆法都认,存在的优先,一个都没有则给第一条候选。
    #[test]
    fn default_lookup_prefers_existing() {
        let base = temp_dir("lookup");
        let packaged = base.join("firmware").join(MERGED_IMAGE);
        let beside = base.join(MERGED_IMAGE);

        // 都没有 → 第一条候选(firmware 子目录),便于提示"默认找的是哪儿"。
        assert_eq!(pick_default(&base), packaged);

        // 只有"跟 exe 摆一起"那份 → 用它。
        std::fs::write(&beside, [ESP_IMAGE_MAGIC]).unwrap();
        assert_eq!(pick_default(&base), beside);

        // 子目录那份也在 → 优先子目录(交付包的摆法)。
        std::fs::create_dir_all(base.join("firmware")).unwrap();
        std::fs::write(&packaged, [ESP_IMAGE_MAGIC]).unwrap();
        assert_eq!(pick_default(&base), packaged);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 对话框预选:只认真实存在的文件。软件目录里翻不到任何镜像时必须给 None,
    /// 前端于是不传 `defaultPath` —— 不给用户预填一个不存在的文件名。
    #[test]
    fn dialog_default_requires_existing_file() {
        let base = temp_dir("dialog");
        let missing = base.join("nope.bin");
        assert_eq!(dialog_default_in(&base, &missing), None);

        // 生效路径存在 → 预选它本人。
        let real = base.join("real.bin");
        std::fs::write(&real, [ESP_IMAGE_MAGIC]).unwrap();
        assert_eq!(dialog_default_in(&base, &real), Some(real.clone()));

        // 生效路径没了、但软件目录里有交付镜像 → 预选那条存在的。
        let packaged = base.join("firmware").join(MERGED_IMAGE);
        std::fs::create_dir_all(base.join("firmware")).unwrap();
        std::fs::write(&packaged, [ESP_IMAGE_MAGIC]).unwrap();
        assert_eq!(dialog_default_in(&base, &missing), Some(packaged));

        // 连交付镜像也不在 → 依旧 None(不许拿不存在的路径当默认值)。
        std::fs::remove_file(base.join("firmware").join(MERGED_IMAGE)).unwrap();
        assert_eq!(dialog_default_in(&base, &missing), None);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 校验的三条硬规则:魔数不对要拒绝、空文件要拒绝、正常镜像要通过并算出摘要。
    #[test]
    fn firmware_inspection_rules() {
        let dir = temp_dir("inspect");

        let missing = inspect(&dir.join("not-there.bin"));
        assert!(!missing.exists);
        assert!(missing.error.is_some());

        // 软件目录里默认那份没找到时,"找不到"要说人话:指出找过哪儿、让用户自己选。
        let no_default = inspect(&candidates_in(&exe_dir())[0]);
        assert!(!no_default.exists);
        let hint = no_default.error.clone().unwrap();
        assert!(hint.contains("选择 .bin"), "提示没说怎么自救: {hint}");
        assert_eq!(no_default.default_path, None, "不存在的镜像不许当默认预选值");

        let empty = dir.join("empty.bin");
        std::fs::write(&empty, []).unwrap();
        let info = inspect(&empty);
        assert!(info.exists && info.size == 0);
        assert!(info.error.is_some());

        let bogus = dir.join("bogus.bin");
        std::fs::write(&bogus, [0x00u8, 0x01, 0x02, 0x03]).unwrap();
        let info = inspect(&bogus);
        assert!(!info.magic_ok);
        assert!(info.error.unwrap().contains("0xE9"));

        let good = dir.join("good.bin");
        let merged = crate::firmware_image::tests::merged();
        std::fs::write(&good, &merged).unwrap();
        let info = inspect(&good);
        assert!(info.magic_ok && info.error.is_none() && info.size == merged.len() as u64);
        assert_eq!(
            info.sha256,
            format!("{:x}", Sha256::digest(&merged))
        );

        // 跨读窗口的大文件:摘要必须覆盖全部字节(读循环漏字节时这条会失败)。
        let big = dir.join("big.bin");
        let mut body = vec![0x5Au8; 200 * 1024];
        body[0] = ESP_IMAGE_MAGIC;
        std::fs::write(&big, &body).unwrap();
        let info = inspect(&big);
        assert!(info.magic_ok && info.size == body.len() as u64);
        assert_eq!(info.sha256, format!("{:x}", Sha256::digest(&body)));

        // 只改最后一个字节 → 摘要必须不同(证明尾部也进了哈希)。
        let mut tweaked = body.clone();
        let last = tweaked.len() - 1;
        tweaked[last] ^= 0xFF;
        let tweaked_path = dir.join("big-tweaked.bin");
        std::fs::write(&tweaked_path, &tweaked).unwrap();
        assert_ne!(inspect(&tweaked_path).sha256, info.sha256);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_image_is_rejected_before_port_access() {
        let dir = temp_dir("preflight");
        let path = dir.join("app-only.bin");
        std::fs::write(&path, [ESP_IMAGE_MAGIC, 1, 2, 3]).unwrap();
        assert!(inspect(&path).error.is_some());
        let mut events = 0;
        let error = flash(&FlashRequest {
            port: "not-a-real-port", path: &path, baud: DEFAULT_BAUD,
            verify: true, erase: false, chip: Chip::Esp32c3,
        }, &mut |_| events += 1).unwrap_err();
        assert!(error.contains("镜像"));
        assert_eq!(events, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
