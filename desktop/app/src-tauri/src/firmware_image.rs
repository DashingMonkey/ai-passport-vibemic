//! Validate the supported ESP32-C3 merged image before any serial-port access.
//! The desktop flasher uses the board's standard 0x8000 partition-table offset.
use md5::Md5;
use sha2::{Digest, Sha256};

const FLASH_SIZE: usize = 8 * 1024 * 1024;
const TABLE: usize = 0x8000;
const TABLE_SIZE: usize = 0xc00;

fn word(bytes: &[u8], at: usize) -> usize {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
}

/// Checks segment bounds, chip ID, XOR checksum and optional appended SHA-256.
fn image_end(data: &[u8], start: usize, limit: usize, app: bool) -> Result<usize, String> {
    let bytes = data
        .get(start..limit.min(data.len()))
        .ok_or("镜像偏移越界")?;
    if bytes.len() < 24 || bytes[0] != 0xe9 {
        return Err("ESP 镜像头缺失或截断 (0xE9)".into());
    }
    if bytes[1] == 0 || bytes[1] > 16 || bytes[12..14] != [5, 0] || bytes[23] > 1 {
        return Err("镜像不是有效的 ESP32-C3 固件".into());
    }
    let mut cursor = 24;
    let mut checksum = 0xef;
    for segment in 0..bytes[1] {
        if cursor + 8 > bytes.len() {
            return Err("镜像段头截断".into());
        }
        let len = word(bytes, cursor + 4);
        cursor += 8;
        let end = cursor.checked_add(len).ok_or("镜像段长度溢出")?;
        let payload = bytes.get(cursor..end).ok_or("镜像段超出文件或分区边界")?;
        if app && segment == 0 && (payload.len() < 256 || payload[..4] != [0x32, 0x54, 0xcd, 0xab])
        {
            return Err("应用镜像缺少 ESP-IDF 描述信息".into());
        }
        for byte in payload {
            checksum ^= byte;
        }
        cursor = end;
    }
    // Checksum is the last byte of the next 16-byte aligned image boundary.
    let end = (cursor + 16) & !15;
    if bytes.get(end - 1) != Some(&checksum) {
        return Err("镜像校验和错误或文件截断".into());
    }
    let mut total = end;
    if bytes[23] == 1 {
        let hash = bytes.get(end..end + 32).ok_or("镜像 SHA-256 截断")?;
        if Sha256::digest(&bytes[..end]).as_slice() != hash {
            return Err("镜像 SHA-256 校验失败".into());
        }
        total += 32;
    }
    Ok(start + total)
}

pub fn validate(data: &[u8]) -> Result<(), String> {
    if data.is_empty() {
        return Err("固件文件是空的".into());
    }
    if data[0] != 0xe9 {
        return Err("这不是 ESP 镜像(首字节不是 0xE9)".into());
    }
    if data.len() > FLASH_SIZE {
        return Err("镜像超过设备 8 MB Flash 容量".into());
    }
    image_end(data, 0, TABLE, false)?;
    let table = data
        .get(TABLE..TABLE + TABLE_SIZE)
        .ok_or("缺少 0x8000 分区表,请选择完整的 -full.bin 合并镜像")?;
    let mut partitions = Vec::new();
    let mut md5_ok = false;
    for cursor in (0..TABLE_SIZE).step_by(32) {
        let entry = &table[cursor..cursor + 32];
        match &entry[..2] {
            [0xeb, 0xeb] => {
                if Md5::digest(&table[..cursor]).as_slice() != &entry[16..32] {
                    return Err("分区表 MD5 校验失败".into());
                }
                md5_ok = true;
                break;
            }
            [0xaa, 0x50] => {}
            _ => return Err("分区表不完整或缺少 MD5 校验".into()),
        }
        let offset = word(entry, 4);
        let size = word(entry, 8);
        let end = offset.checked_add(size).ok_or("分区边界溢出")?;
        let alignment = if entry[2] == 0 { 0x10000 } else { 0x1000 };
        if size == 0 || offset < TABLE + 0x1000 || end > FLASH_SIZE || offset % alignment != 0 {
            return Err("分区边界或对齐无效".into());
        }
        if partitions
            .iter()
            .any(|&(lo, hi, _, _)| offset < hi && end > lo)
        {
            return Err("分区互相重叠".into());
        }
        partitions.push((offset, end, entry[2], entry[3]));
    }
    if !md5_ok {
        return Err("分区表缺少 MD5 校验".into());
    }
    // Factory is the bootable application in this product's merged delivery.
    let factory: Vec<_> = partitions.iter().filter(|p| p.2 == 0 && p.3 == 0).collect();
    if factory.len() != 1 {
        return Err("合并镜像必须包含一个 factory 应用分区".into());
    }
    image_end(data, factory[0].0, factory[0].1, true)?;
    // If other app slots contain data, they too must hold complete valid images.
    for &(start, end, kind, subtype) in &partitions {
        if kind == 0 && subtype != 0 && data.get(start).is_some_and(|&b| b != 0xff) {
            image_end(data, start, end, true)?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn image(app: bool) -> Vec<u8> {
        let mut out = vec![0; 24];
        out[0] = 0xe9;
        out[1] = 1;
        out[12] = 5;
        out[23] = 1;
        let mut payload = vec![0; if app { 256 } else { 16 }];
        if app {
            payload[..4].copy_from_slice(&[0x32, 0x54, 0xcd, 0xab]);
        }
        out.extend_from_slice(&0x3fc80000u32.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        out.resize((out.len() + 16) & !15, 0);
        *out.last_mut().unwrap() = payload.iter().fold(0xef, |a, b| a ^ b);
        let hash = Sha256::digest(&out);
        out.extend_from_slice(&hash);
        out
    }

    pub fn merged() -> Vec<u8> {
        let mut out = image(false);
        out.resize(0x10000, 0xff);
        let entry = &mut out[TABLE..TABLE + 32];
        entry.fill(0);
        entry[..2].copy_from_slice(&[0xaa, 0x50]);
        entry[4..8].copy_from_slice(&0x10000u32.to_le_bytes());
        entry[8..12].copy_from_slice(&0x7f0000u32.to_le_bytes());
        entry[12..19].copy_from_slice(b"factory");
        let hash = Md5::digest(entry);
        out[TABLE + 32..TABLE + 34].copy_from_slice(&[0xeb, 0xeb]);
        out[TABLE + 48..TABLE + 64].copy_from_slice(&hash);
        out.extend_from_slice(&image(true));
        out
    }

    #[test]
    fn accepts_complete_c3_merged_image() {
        validate(&merged()).unwrap();
    }

    #[test]
    fn rejects_app_only_boot_only_truncation_and_corruption() {
        assert!(validate(&image(true)).is_err());
        assert!(validate(&image(false)).is_err());
        assert!(validate(&[0xe9, 1, 2, 3]).is_err());
        let good = merged();
        for end in [
            0,
            1,
            23,
            TABLE,
            TABLE + TABLE_SIZE - 1,
            0x10000,
            good.len() - 1,
        ] {
            assert!(
                validate(&good[..end]).is_err(),
                "accepted truncation at {end}"
            );
        }
        for at in [
            12,
            32,
            TABLE + 8,
            TABLE + 48,
            0x10000 + 12,
            0x10000 + 40,
            good.len() - 1,
        ] {
            let mut bad = good.clone();
            bad[at] ^= 1;
            assert!(validate(&bad).is_err(), "accepted corruption at {at}");
        }
    }

    #[test]
    fn rejects_invalid_partition_bounds_even_with_valid_md5() {
        for (offset, size) in [(0x8000u32, 0x10000u32), (0x10000, 0x800000), (0x10000, 16)] {
            let mut bad = merged();
            bad[TABLE + 4..TABLE + 8].copy_from_slice(&offset.to_le_bytes());
            bad[TABLE + 8..TABLE + 12].copy_from_slice(&size.to_le_bytes());
            let hash = Md5::digest(&bad[TABLE..TABLE + 32]);
            bad[TABLE + 48..TABLE + 64].copy_from_slice(&hash);
            assert!(validate(&bad).is_err());
        }
    }
}
