//! Profile JSON 模型(桌面端为权威;设备端只消费)。

use crate::frame::{frame_type, Frame};
use serde::{Deserialize, Serialize};

pub const SCHEMA_ID: &str = "vibemic-profile-1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub id: String,
    pub name: String,
    /// 递增;冲突时 PC 端覆盖。
    pub version: u32,
    /// 匹配规则(桌面端为权威;设备忽略)。
    #[serde(rename = "match", default)]
    pub match_rules: MatchRules,
    pub keys: KeyMap,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchRules {
    #[serde(default)]
    pub windows: Vec<MatchEntry>,
    #[serde(default)]
    pub macos: Vec<MatchEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_regex: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyMap {
    #[serde(default)]
    pub ok_press: Vec<ActionStep>,
    #[serde(default)]
    pub ok_release: Vec<ActionStep>,
    #[serde(default)]
    pub up_click: Vec<ActionStep>,
    #[serde(default)]
    pub up_long: Vec<ActionStep>,
    #[serde(default)]
    pub down_click: Vec<ActionStep>,
}

/// 动作项:`tap`(按下+抬起)/`down`/`up`/`delayMs`/`replay`(重放另一槽)。
/// 按住式语音 = okPress `down` + okRelease `up`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ActionStep {
    Key(KeyAction),
    Delay {
        #[serde(rename = "delayMs")]
        delay_ms: u16,
    },
    Replay {
        replay: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyAction {
    #[serde(default)]
    pub mods: Vec<Modifier>,
    pub key: String,
    #[serde(default = "default_mode")]
    pub mode: KeyMode,
}

fn default_mode() -> KeyMode {
    KeyMode::Tap
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    /// Win 键(macOS ⌘,HID GUI)。注意:不提供 Fn/Globe。
    Win,
}

impl Modifier {
    /// HID 键盘报文修饰键位。
    pub fn hid_bit(self) -> u8 {
        match self {
            Modifier::Ctrl => 0x01,
            Modifier::Shift => 0x02,
            Modifier::Alt => 0x04,
            Modifier::Win => 0x08,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyMode {
    Tap,
    Down,
    Up,
}

impl Profile {
    /// 反序列化并做结构性校验(schema id、键名与 replay 引用)。
    pub fn parse(json: &str) -> Result<Profile, String> {
        let p: Profile = serde_json::from_str(json).map_err(|e| e.to_string())?;
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA_ID {
            return Err(format!(
                "unknown $schema: {} (expect {SCHEMA_ID})",
                self.schema
            ));
        }
        if self.id.is_empty() || self.name.is_empty() {
            return Err("profile id/name must not be empty".into());
        }
        let slots = [
            ("okPress", &self.keys.ok_press),
            ("okRelease", &self.keys.ok_release),
            ("upClick", &self.keys.up_click),
            ("upLong", &self.keys.up_long),
            ("downClick", &self.keys.down_click),
        ];
        for (name, steps) in slots {
            for step in steps {
                if let ActionStep::Key(k) = step {
                    if hid_keycode(&k.key).is_none() {
                        return Err(format!("{name}: unknown key {:?}", k.key));
                    }
                    if k.mods.len() > 8 {
                        return Err(format!("{name}: too many modifiers"));
                    }
                }
            }
        }
        Ok(())
    }

    /// 序列化为紧凑 JSON(设备分块传输用)。
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| e.to_string())
    }
}

/// HID usage-page 0x07 键码子集(与设备 `vm_profile.h` 常量一致)。
/// 单字符 = 字母/数字;具名键见 match 分支。
pub fn hid_keycode(name: &str) -> Option<u8> {
    let bytes = name.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_lowercase() {
        return Some(0x04 + (bytes[0] - b'a'));
    }
    if bytes.len() == 1 && bytes[0].is_ascii_digit() {
        return Some(if bytes[0] == b'0' {
            0x27
        } else {
            0x1E + (bytes[0] - b'1')
        });
    }
    Some(match name {
        "enter" | "return" => 0x28,
        "esc" => 0x29,
        "backspace" => 0x2A,
        "tab" => 0x2B,
        "space" => 0x2C,
        "delete" | "del" => 0x4C,
        "insert" => 0x49,
        "home" => 0x4A,
        "end" => 0x4D,
        "pageup" => 0x4B,
        "pagedown" => 0x4E,
        "up" => 0x52,
        "down" => 0x51,
        "left" => 0x50,
        "right" => 0x4F,
        "f1" => 0x3A,
        "f2" => 0x3B,
        "f3" => 0x3C,
        "f4" => 0x3D,
        "f5" => 0x3E,
        "f6" => 0x3F,
        "f7" => 0x40,
        "f8" => 0x41,
        "f9" => 0x42,
        "f10" => 0x43,
        "f11" => 0x44,
        "f12" => 0x45,
        _ => return None,
    })
}

/// 把 profile JSON 切成 PROFILE_CHUNK 帧序列(seq 从 0 计,total 为分块总数)。
/// 单块 data ≤ 255B(设备端头里的 dataLen 为 1 字节)。
pub fn chunk_profile(json: &str, profile_id: u8, max_chunk: usize) -> Result<Vec<Frame>, String> {
    if max_chunk == 0 || max_chunk > 255 {
        return Err("max_chunk must be 1..=255".into());
    }
    let data = json.as_bytes();
    if data.len() > 255usize * 65535 {
        return Err("profile too large".into());
    }
    let total = data.len().div_ceil(max_chunk);
    if total > u16::MAX as usize {
        return Err("profile too large for chunk header".into());
    }
    let mut frames = Vec::with_capacity(total);
    for (seq, part) in data.chunks(max_chunk).enumerate() {
        let mut payload = Vec::with_capacity(6 + part.len());
        payload.push(profile_id);
        payload.extend_from_slice(&(seq as u16).to_le_bytes());
        payload.extend_from_slice(&(total as u16).to_le_bytes());
        payload.push(part.len() as u8);
        payload.extend_from_slice(part);
        frames.push(Frame::new(frame_type::PROFILE_CHUNK, payload));
    }
    Ok(frames)
}

/// 分块重组(桌面侧对照实现/自检用)。
pub fn assemble_chunks(chunks: &[&[u8]]) -> Result<Vec<u8>, String> {
    let mut assembled = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let (head, data) = crate::payload::ChunkHeader::parse(c).map_err(|e| format!("{e:?}"))?;
        if head.seq as usize != i {
            return Err(format!("chunk seq {} out of order (expect {i})", head.seq));
        }
        if head.total as usize != chunks.len() {
            return Err(format!("chunk total {} != {}", head.total, chunks.len()));
        }
        assembled.extend_from_slice(data);
    }
    Ok(assembled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 示例 profile(字段名为协议契约)。
    const EXAMPLE_PROFILE: &str = r#"{
      "$schema": "vibemic-profile-1",
      "id": "trae-win",
      "name": "Trae",
      "version": 3,
      "match": {
        "windows": [{ "exe": "Trae.exe" }, { "titleRegex": "Trae" }],
        "macos":   [{ "bundleId": "com.trae.app" }]
      },
      "keys": {
        "okPress":   [ { "mods": ["ctrl","alt"], "key": "v", "mode": "tap" } ],
        "okRelease": [ { "replay": "okPress" } ],
        "upClick":   [ { "key": "enter", "mode": "tap" } ],
        "upLong":    [ { "mods": ["ctrl"], "key": "enter", "mode": "tap" } ],
        "downClick": [ { "key": "esc", "mode": "tap" } ]
      }
    }"#;

    #[test]
    fn parses_example_profile() {
        let p = Profile::parse(EXAMPLE_PROFILE).unwrap();
        assert_eq!(p.id, "trae-win");
        assert_eq!(p.name, "Trae");
        assert_eq!(p.version, 3);
        assert_eq!(p.match_rules.windows.len(), 2);
        assert_eq!(
            p.match_rules.macos[0].bundle_id.as_deref(),
            Some("com.trae.app")
        );
        assert_eq!(p.keys.ok_press.len(), 1);
        assert_eq!(
            p.keys.ok_press[0],
            ActionStep::Key(KeyAction {
                mods: vec![Modifier::Ctrl, Modifier::Alt],
                key: "v".into(),
                mode: KeyMode::Tap,
            })
        );
        assert_eq!(
            p.keys.ok_release[0],
            ActionStep::Replay {
                replay: "okPress".into()
            }
        );
    }

    #[test]
    fn serializes_camel_case_roundtrip() {
        let p = Profile::parse(EXAMPLE_PROFILE).unwrap();
        let json = p.to_json().unwrap();
        assert!(json.contains("\"okPress\""));
        assert!(json.contains("\"titleRegex\""));
        assert_eq!(Profile::parse(&json).unwrap(), p);
    }

    /// `delayMs` 是协议里仍在用的步骤类型(设备侧两记 Esc 之间就要延时),
    /// 只是 DOWN 长按槽位移除后示例 profile 里不再出现它,所以单独钉住。
    #[test]
    fn delay_step_roundtrip() {
        let json = "{\"delayMs\":30}";
        assert_eq!(
            serde_json::from_str::<ActionStep>(json).unwrap(),
            ActionStep::Delay { delay_ms: 30 }
        );
        assert_eq!(
            serde_json::to_string(&ActionStep::Delay { delay_ms: 30 }).unwrap(),
            json
        );
    }

    #[test]
    fn rejects_unknown_schema_and_keys() {
        let bad = EXAMPLE_PROFILE.replace("vibemic-profile-1", "vibemic-profile-9");
        assert!(Profile::parse(&bad).is_err());
        let bad_key = EXAMPLE_PROFILE.replace("\"key\": \"v\"", "\"key\": \"nostandardkey\"");
        assert!(Profile::parse(&bad_key).is_err());
    }

    #[test]
    fn keycode_table() {
        assert_eq!(hid_keycode("a"), Some(0x04));
        assert_eq!(hid_keycode("v"), Some(0x19));
        assert_eq!(hid_keycode("0"), Some(0x27));
        assert_eq!(hid_keycode("1"), Some(0x1E));
        assert_eq!(hid_keycode("enter"), Some(0x28));
        assert_eq!(hid_keycode("esc"), Some(0x29));
        assert_eq!(hid_keycode("delete"), Some(0x4C));
        assert_eq!(hid_keycode("f12"), Some(0x45));
        assert_eq!(hid_keycode("up"), Some(0x52));
        assert_eq!(hid_keycode("Fn"), None);
        assert_eq!(hid_keycode("aa"), None);
    }

    #[test]
    fn modifier_bits() {
        assert_eq!(Modifier::Ctrl.hid_bit(), 0x01);
        assert_eq!(Modifier::Shift.hid_bit(), 0x02);
        assert_eq!(Modifier::Alt.hid_bit(), 0x04);
        assert_eq!(Modifier::Win.hid_bit(), 0x08);
    }

    #[test]
    fn profile_chunking_roundtrip() {
        let p = Profile::parse(EXAMPLE_PROFILE).unwrap();
        let json = p.to_json().unwrap();
        let frames = chunk_profile(&json, 7, 32).unwrap();
        assert!(frames.len() > 1);
        let payloads: Vec<&[u8]> = frames.iter().map(|f| f.payload.as_slice()).collect();
        assert_eq!(
            assemble_chunks(&payloads).unwrap(),
            json.as_bytes().to_vec()
        );
        for f in &frames {
            assert_eq!(f.kind, frame_type::PROFILE_CHUNK);
            assert!(f.encode().is_ok());
        }
    }
}
