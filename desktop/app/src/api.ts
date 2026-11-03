// 与后端共享的状态负载类型(serde ↔ TS,字段以 src-tauri/src/state.rs 为准)
export type SendKey = "enter" | "ctrl_enter";

/** 固件烧录设置(src-tauri/src/state.rs 的 FirmwareCfg)。 */
export interface FirmwareCfg {
  /** 镜像路径;空串 = 在软件所在目录里找(firmware 子目录或 exe 同目录) */
  path: string;
  baud: number;
  verify: boolean;
}

export interface AppStatePayload {
  connected: boolean;
  battery: number | null;
  fw_ver: number;
  ptt: boolean;
  asr: string; // missing | downloading | ready | recognizing | error: ...
  download_pct: number;
  last_text: string | null;
  send_key: SendKey;
  firmware: FirmwareCfg;
  /** 烧录后设备侧绑定被清掉,需要用户去系统蓝牙设置里重新配对一次 */
  pairing_needed: boolean;
}

/** 待烧镜像的自检结果(src-tauri/src/flash.rs 的 FirmwareInfo)。 */
export interface FirmwareInfo {
  path: string;
  /** exe 所在目录:提示"默认去哪儿找镜像"用 */
  exe_dir: string;
  /**
   * "选择固件"对话框要预选的文件;null = 软件目录里没有镜像,
   * 此时不传 defaultPath,由用户自己选(不预填不存在的文件名)。
   */
  default_path: string | null;
  exists: boolean;
  size: number;
  sha256: string;
  magic_ok: boolean;
  /** 不能烧的原因;null = 可以烧 */
  error: string | null;
}

/** 可选串口(src-tauri/src/flash.rs 的 PortEntry)。 */
export interface PortEntry {
  name: string;
  label: string;
  /** USB 串口且 pid 命中 ESP32 USB-Serial/JTAG(自带下载模式) */
  usb_jtag: boolean;
}

/** 检查更新结果(src-tauri/src/update.rs 的 UpdateInfo)。 */
export interface UpdateInfo {
  /** 本地版本(取自 Tauri 的 package_info = tauri.conf.json 的 version) */
  current: string;
  /** 远端最新版本号(已去 v 前缀与预发布后缀) */
  latest: string;
  /** 远端是否比本地新 */
  newer: boolean;
  /** 实际应答的源:「gitee」|「github」 */
  source: string;
  /** 应答源的发布页(「下载新版本」按钮打开它) */
  page_url: string;
}

/** 本机路径(src-tauri/src/lib.rs 的 AppPaths):面板上展示与「打开目录」都读它 */
export interface AppPaths {
  config_dir: string;
  config_file: string;
  model_dir: string;
  log_dir: string;
}
