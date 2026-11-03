//! VibeMic 控制协议(单一事实源:本 crate;设备端 C 实现位于
//! `device/main/vm_protocol.c`,两侧行为由同一组测试向量约束)。
//!
//! 帧布局:`[0x56 0x4D] magic | [ver] | [type] | [len]u16-LE | [payload] | [crc16]`
//! 其中 CRC16-CCITT(init 0xFFFF,无反射)覆盖 `ver..payload`;帧总长 = 8 + payload。

pub mod audio;
pub mod frame;
pub mod payload;
pub mod profile;

/// 服务与特征值 UUID(Base `a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7aXX`)。
pub mod uuids {
    pub const SERVICE: &str = "a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7a00";
    pub const CHR_VM_CTRL: &str = "a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7a01";
    pub const CHR_VM_PROFILE: &str = "a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7a02";
    pub const CHR_VM_STATUS: &str = "a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7a03";
    pub const CHR_VM_EVENT: &str = "a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7a04";
    pub const CHR_VM_AUDIO: &str = "a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7a05";

    /// 蓝牙广播设备名(与设备固件 `VM_DEVICE_NAME` 一致)。
    pub const DEVICE_NAME: &str = "AI Passport Vibe Mic";

    /// 可接受的设备名变体。
    ///
    /// Windows/macOS 会缓存已配对设备的蓝牙名,系统回报的名字可能与设备当前
    /// 广播的名字不一致;因此匹配时同时接受这些别名(忽略大小写与空白)。
    pub const DEVICE_NAME_ALIASES: [&str; 3] = [
        "AI Passport Vibe Mic",
        "AI Passport VibeMic",
        "Passport VibeMic",
    ];

    /// 名字归一化:去掉所有空白并转小写。
    fn normalized(name: &str) -> String {
        name.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(|c| c.to_lowercase())
            .collect()
    }

    /// 该设备名是否指向 VibeMic(见 [`DEVICE_NAME_ALIASES`])。
    ///
    /// 唯一用途:连接时在系统已知设备里认目标(见 `vibemic-link` 的 `WinRtLink::connect`)。
    /// 判错名字的代价是连不上,所以这里保持**严格** —— 不认关键词、不认模糊包含。
    pub fn device_name_matches(name: &str) -> bool {
        let wanted = normalized(name);
        !wanted.is_empty() && DEVICE_NAME_ALIASES.iter().any(|a| normalized(a) == wanted)
    }
}

#[cfg(test)]
mod tests {
    use super::uuids;

    /// 名字匹配:当前名、变体写法与主机缓存的别名都要能匹配;无关设备名不能误匹配。
    #[test]
    fn device_name_matching_accepts_aliases() {
        assert_eq!(uuids::DEVICE_NAME, "AI Passport Vibe Mic");
        assert!(uuids::device_name_matches("AI Passport Vibe Mic"));
        assert!(uuids::device_name_matches("ai passport vibemic")); // 大小写 + 空格归一化
        assert!(uuids::device_name_matches("AI Passport VibeMic"));
        assert!(uuids::device_name_matches("Passport VibeMic")); // 主机缓存的别名
        assert!(!uuids::device_name_matches(""));
        assert!(!uuids::device_name_matches("   "));
        assert!(!uuids::device_name_matches("Passport Vibe"));
        assert!(!uuids::device_name_matches("Some Other Keyboard"));
        // 别家设备的名字里带 passport 也不算:严格匹配是对表,不是关键词搜索。
        assert!(!uuids::device_name_matches("AI Passport"));
        assert!(!uuids::device_name_matches("FoloToy AI Passport"));
    }
}
