//! USB 串口兜底通道(AI Passport 的 USB-Serial-JTAG 口):同一帧格式,每帧后
//! 追加 `\n` 分隔。设备端串口帧服务尚未实现;本实现先行落地桌面侧,便于协议联调。

use std::io::{Read, Write};
use std::time::Duration;

use serialport::{SerialPort, SerialPortType};

use vibemic_protocol::frame::{Decoder, Frame};

use crate::{DeviceLink, LinkError};

pub const DEFAULT_BAUD: u32 = 115200;

pub struct SerialLink {
    port: Box<dyn SerialPort>,
    decoder: Decoder,
}

impl SerialLink {
    pub fn open(path: &str, baud: u32) -> Result<SerialLink, LinkError> {
        let port = serialport::new(path, baud)
            .timeout(Duration::from_millis(50))
            .open()
            .map_err(|e| LinkError::Io(format!("open {path}: {e}")))?;
        Ok(SerialLink {
            port,
            decoder: Decoder::new(),
        })
    }

    /// 列出本机候选串口(AI Passport 会以 USB-Serial-JTAG 形式出现)。
    pub fn list_ports() -> Result<Vec<String>, LinkError> {
        serialport::available_ports()
            .map(|ports| {
                ports
                    .into_iter()
                    .filter(|p| p.port_type != SerialPortType::PciPort)
                    .map(|p| p.port_name)
                    .collect()
            })
            .map_err(|e| LinkError::Io(e.to_string()))
    }
}

impl DeviceLink for SerialLink {
    fn name(&self) -> &'static str {
        "serial"
    }

    fn send(&mut self, frame: &Frame) -> Result<(), LinkError> {
        let mut bytes = frame
            .encode()
            .map_err(|e| LinkError::Protocol(format!("{e:?}")))?;
        bytes.push(b'\n'); // 串口通道帧分隔
        self.port
            .write_all(&bytes)
            .and_then(|_| self.port.flush())
            .map_err(|e| LinkError::Io(e.to_string()))
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Frame>, LinkError> {
        let port = &mut self.port;
        poll_bytes(&mut self.decoder, timeout, |wait, byte| {
            port.set_timeout(wait)?;
            port.read(byte)
        })
    }
}

/// All bytes go through the binary decoder, including newline-valued payload/CRC.
fn poll_bytes(
    decoder: &mut Decoder,
    timeout: Duration,
    mut read: impl FnMut(Duration, &mut [u8]) -> std::io::Result<usize>,
) -> Result<Option<Frame>, LinkError> {
    let deadline = std::time::Instant::now() + timeout;
    let mut byte = [0u8; 1];
    loop {
        // 期限到了就按"本轮没有帧"返回(DeviceLink 的约定:超时 = Ok(None),
        // 不能报错误 —— 否则 wait_for / vmctl watch 会把正常空闲当致命错误)。
        let Some(remain) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return Ok(None);
        };
        match read(remain.min(Duration::from_millis(50)), &mut byte) {
            Ok(0) => return Err(LinkError::Io("serial stream closed".into())),
            Ok(_) => {
                // 帧内 0x0A 是合法数据;帧外分隔符由解码器的 magic 搜索跳过。
                if let Some(frame) = decoder.feed(byte[0]) {
                    return Ok(Some(frame));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                if std::time::Instant::now() >= deadline {
                    return Ok(None);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(LinkError::Io(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_poll_preserves_payload_length_crc_and_frame_delimiters() {
        let frames: Vec<_> = (0..=255).map(|b| Frame::new(0x13, vec![b; 10])).collect();
        let mut bytes = Vec::new();
        for frame in &frames {
            bytes.extend(frame.encode().unwrap());
            bytes.push(b'\n');
        }
        let mut input = std::io::Cursor::new(bytes);
        let mut decoder = Decoder::new();
        for frame in frames {
            assert_eq!(
                poll_bytes(&mut decoder, Duration::from_secs(1), |_, byte| input
                    .read(byte))
                .unwrap(),
                Some(frame)
            );
        }
        assert!(
            poll_bytes(&mut decoder, Duration::from_secs(1), |_, byte| input
                .read(byte))
            .is_err()
        );
    }
}
