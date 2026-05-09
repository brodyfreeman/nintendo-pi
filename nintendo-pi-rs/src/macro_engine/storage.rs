//! Macro storage: binary logical input format + JSON index CRUD.
//!
//! Runtime format (MAC3):
//!   Header (20 bytes):
//!     [0..4]   Magic "MAC3"
//!     [4..6]   Version (u16 LE) = 3
//!     [6..8]   Frame size (u16 LE) = 20
//!     [8..12]  Frame count (u32 LE)
//!     [12..20] Duration microseconds (u64 LE)
//!
//!   Per frame (20 bytes):
//!     [0..8]   Timestamp microseconds (u64 LE)
//!     [8..12]  Canonical button bitmask (u32 LE)
//!     [12..14] Left stick X, normalized percent * 100 (i16 LE)
//!     [14..16] Left stick Y
//!     [16..18] Right stick X
//!     [18..20] Right stick Y

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::calibration::{
    auto_calibrate_centers, StickCalibrator, StickPair, C_STICK_CAL, DEFAULT_DEADZONE,
    MAIN_STICK_CAL,
};
use crate::input::{parse_hid_report, ButtonState, LogicalInput};

pub const MAGIC: &[u8; 4] = b"MAC3";
pub const FORMAT_VERSION: u16 = 3;
pub const HEADER_SIZE: usize = 20;
pub const FRAME_SIZE: usize = 20;

const OLD_MAGIC_MAC2: &[u8; 4] = b"MAC2";
const OLD_MAGIC_MACO: &[u8; 4] = b"MACO";
const OLD_HEADER_SIZE: usize = 16;
const OLD_REPORT_SIZE: u16 = 64;
const OLD_FRAME_SIZE: usize = 8 + OLD_REPORT_SIZE as usize;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroEntry {
    pub id: u32,
    pub name: String,
    pub filename: String,
    pub frame_count: u32,
    pub duration_ms: u32,
    pub created: String,
}

fn index_path(macros_dir: &Path) -> PathBuf {
    macros_dir.join("index.json")
}

pub fn load_index(macros_dir: &Path) -> Vec<MacroEntry> {
    let path = index_path(macros_dir);
    if !path.exists() {
        return Vec::new();
    }
    match fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(e) => {
            error!("[MACRO] Failed to read index: {e}");
            Vec::new()
        }
    }
}

pub fn save_index(macros_dir: &Path, index: &[MacroEntry]) {
    fs::create_dir_all(macros_dir).ok();
    let path = index_path(macros_dir);
    match serde_json::to_string_pretty(index) {
        Ok(data) => {
            if let Err(e) = fs::write(&path, data) {
                error!("[MACRO] Failed to write index: {e}");
            }
        }
        Err(e) => error!("[MACRO] Failed to serialize index: {e}"),
    }
}

fn next_id(index: &[MacroEntry]) -> u32 {
    index.iter().map(|e| e.id).max().unwrap_or(0) + 1
}

/// Back up and convert old raw HID macro files to the MAC3 logical format.
///
/// This is the only startup path that understands MACO/MAC2. Once startup
/// migration completes, the player and recorder only deal in MAC3 files.
pub fn migrate_old_macros(macros_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let index = load_index(macros_dir);
    if index.is_empty() {
        return Ok(None);
    }

    let old_entries = index
        .iter()
        .filter(|entry| is_old_macro_file(&macros_dir.join(&entry.filename)))
        .count();

    if old_entries == 0 {
        return Ok(None);
    }

    let backup_dir = backup_macros_dir(macros_dir)?;
    info!(
        "[MACRO] Migrating {old_entries} old macro file(s); backup at {}",
        backup_dir.display()
    );

    for entry in &index {
        let path = macros_dir.join(&entry.filename);
        if !is_old_macro_file(&path) {
            continue;
        }

        let old_frames = read_old_macro_frames(&path)?;
        let logical_frames = convert_old_frames(&old_frames);
        write_macro_file(&path, &logical_frames)?;
        info!(
            "[MACRO] Migrated macro {} \"{}\" ({} frames)",
            entry.id,
            entry.name,
            logical_frames.len()
        );
    }

    Ok(Some(backup_dir))
}

/// Save recorded logical frames to a binary file and update the index.
/// Returns the macro ID.
pub fn save_macro(
    macros_dir: &Path,
    frames: &[(u64, LogicalInput)],
    name: Option<&str>,
) -> Option<u32> {
    if frames.is_empty() {
        warn!("[MACRO] save_macro called with 0 frames, skipping");
        return None;
    }

    fs::create_dir_all(macros_dir).ok();
    let mut index = load_index(macros_dir);
    let id = next_id(&index);
    let default_name = format!("macro_{id}");
    let name = name.unwrap_or(&default_name).to_string();
    let filename = format!("{id:03}_{name}.bin");
    let filepath = macros_dir.join(&filename);

    if let Err(e) = write_macro_file(&filepath, frames) {
        error!("[MACRO] Failed to write macro file: {e}");
        return None;
    }

    let frame_count = frames.len() as u32;
    let duration_us = frames.last().map(|(ts, _)| *ts).unwrap_or(0);
    let entry = MacroEntry {
        id,
        name,
        filename,
        frame_count,
        duration_ms: (duration_us / 1000).min(u32::MAX as u64) as u32,
        created: unix_timestamp_secs(),
    };
    index.push(entry);
    save_index(macros_dir, &index);

    info!("[MACRO] Saved macro {id} ({frame_count} frames, {duration_us}us)");
    Some(id)
}

pub fn list_macros(macros_dir: &Path) -> Vec<MacroEntry> {
    load_index(macros_dir)
}

pub fn get_macro_info(macros_dir: &Path, macro_id: u32) -> Option<MacroEntry> {
    load_index(macros_dir)
        .into_iter()
        .find(|e| e.id == macro_id)
}

pub fn rename_macro(macros_dir: &Path, macro_id: u32, new_name: &str) -> bool {
    let mut index = load_index(macros_dir);
    if let Some(entry) = index.iter_mut().find(|e| e.id == macro_id) {
        let old_name = entry.name.clone();
        let old_path = macros_dir.join(&entry.filename);
        let new_filename = format!("{:03}_{}.bin", macro_id, new_name);
        let new_path = macros_dir.join(&new_filename);
        if old_path.exists() {
            if let Err(e) = fs::rename(&old_path, &new_path) {
                error!(
                    "[MACRO] Failed to rename file {:?} -> {:?}: {e}",
                    old_path, new_path
                );
            }
        }
        entry.name = new_name.to_string();
        entry.filename = new_filename;
        save_index(macros_dir, &index);
        info!("[MACRO] Renamed macro {macro_id} \"{old_name}\" -> \"{new_name}\"");
        true
    } else {
        warn!("[MACRO] Rename failed: macro {macro_id} not found in index");
        false
    }
}

pub fn delete_macro(macros_dir: &Path, macro_id: u32) -> bool {
    let mut index = load_index(macros_dir);
    let count_before_delete = index.len();

    index.retain(|entry| {
        if entry.id == macro_id {
            let path = macros_dir.join(&entry.filename);
            if let Err(e) = fs::remove_file(&path) {
                warn!("[MACRO] Failed to remove file {}: {e}", entry.filename);
            }
            info!(
                "[MACRO] Deleted macro {macro_id} \"{}\" ({})",
                entry.name, entry.filename
            );
            false
        } else {
            true
        }
    });

    let deleted = index.len() < count_before_delete;
    if deleted {
        save_index(macros_dir, &index);
        debug!("[MACRO] Index updated: {} macro(s) remaining", index.len());
    } else {
        warn!("[MACRO] Delete failed: macro {macro_id} not found in index");
    }
    deleted
}

pub fn get_slot_count(macros_dir: &Path) -> usize {
    load_index(macros_dir).len()
}

pub fn get_macro_id_by_slot(macros_dir: &Path, slot: usize) -> Option<u32> {
    let index = load_index(macros_dir);
    index.get(slot).map(|e| e.id)
}

pub fn decode_frame(bytes: &[u8]) -> Option<(u64, LogicalInput)> {
    if bytes.len() < FRAME_SIZE {
        return None;
    }

    let timestamp_us = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
    let buttons_mask = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let left_x = decode_axis(i16::from_le_bytes(bytes[12..14].try_into().ok()?));
    let left_y = decode_axis(i16::from_le_bytes(bytes[14..16].try_into().ok()?));
    let right_x = decode_axis(i16::from_le_bytes(bytes[16..18].try_into().ok()?));
    let right_y = decode_axis(i16::from_le_bytes(bytes[18..20].try_into().ok()?));

    Some((
        timestamp_us,
        LogicalInput::from_parts(
            ButtonState::from_mask(buttons_mask),
            (left_x, left_y),
            (right_x, right_y),
        ),
    ))
}

fn write_macro_file(path: &Path, frames: &[(u64, LogicalInput)]) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("bin.tmp");
    fs::write(&tmp_path, serialize_macro(frames))?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn serialize_macro(frames: &[(u64, LogicalInput)]) -> Vec<u8> {
    let duration_us = frames.last().map(|(ts, _)| *ts).unwrap_or(0);
    let mut data = Vec::with_capacity(HEADER_SIZE + frames.len() * FRAME_SIZE);

    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    data.extend_from_slice(&(FRAME_SIZE as u16).to_le_bytes());
    data.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    data.extend_from_slice(&duration_us.to_le_bytes());

    for (timestamp_us, input) in frames {
        data.extend_from_slice(&timestamp_us.to_le_bytes());
        data.extend_from_slice(&input.buttons.to_mask().to_le_bytes());
        data.extend_from_slice(&encode_axis(input.left_stick.0).to_le_bytes());
        data.extend_from_slice(&encode_axis(input.left_stick.1).to_le_bytes());
        data.extend_from_slice(&encode_axis(input.right_stick.0).to_le_bytes());
        data.extend_from_slice(&encode_axis(input.right_stick.1).to_le_bytes());
    }

    data
}

fn encode_axis(value: f64) -> i16 {
    (value.clamp(-100.0, 100.0) * 100.0).round() as i16
}

fn decode_axis(value: i16) -> f64 {
    value as f64 / 100.0
}

fn is_old_macro_file(path: &Path) -> bool {
    read_magic(path)
        .map(|magic| &magic == OLD_MAGIC_MAC2 || &magic == OLD_MAGIC_MACO)
        .unwrap_or(false)
}

fn read_magic(path: &Path) -> Option<[u8; 4]> {
    let data = fs::read(path).ok()?;
    if data.len() < 4 {
        return None;
    }
    data[0..4].try_into().ok()
}

fn backup_macros_dir(macros_dir: &Path) -> anyhow::Result<PathBuf> {
    let parent = macros_dir.parent().unwrap_or_else(|| Path::new("."));
    let name = macros_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("macros");
    let base = parent.join(format!("{name}.backup-{}", unix_timestamp_secs()));
    let mut backup = base.clone();
    let mut suffix = 1;
    while backup.exists() {
        backup = parent.join(format!(
            "{}-{}",
            base.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("macros.backup"),
            suffix
        ));
        suffix += 1;
    }
    copy_dir_all(macros_dir, &backup)?;
    Ok(backup)
}

fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn read_old_macro_frames(path: &Path) -> anyhow::Result<Vec<(u64, [u8; 64])>> {
    let data = fs::read(path)?;
    if data.len() < OLD_HEADER_SIZE {
        anyhow::bail!("old macro file too small: {}", path.display());
    }

    let magic = &data[0..4];
    if magic != OLD_MAGIC_MAC2 && magic != OLD_MAGIC_MACO {
        anyhow::bail!("not an old macro file: {}", path.display());
    }

    let report_size = u16::from_le_bytes(data[6..8].try_into()?);
    if report_size != OLD_REPORT_SIZE {
        anyhow::bail!(
            "unsupported old report size in {}: {report_size}",
            path.display()
        );
    }

    let frame_count = u32::from_le_bytes(data[8..12].try_into()?) as usize;
    let expected_len = OLD_HEADER_SIZE + frame_count * OLD_FRAME_SIZE;
    if data.len() < expected_len {
        anyhow::bail!(
            "truncated old macro file {}: expected {expected_len} bytes, got {}",
            path.display(),
            data.len()
        );
    }

    let mut frames = Vec::with_capacity(frame_count);
    for index in 0..frame_count {
        let offset = OLD_HEADER_SIZE + index * OLD_FRAME_SIZE;
        let timestamp_us = u64::from_le_bytes(data[offset..offset + 8].try_into()?);
        let mut report = [0u8; 64];
        report.copy_from_slice(&data[offset + 8..offset + 8 + 64]);
        frames.push((timestamp_us, report));
    }
    Ok(frames)
}

fn convert_old_frames(old_frames: &[(u64, [u8; 64])]) -> Vec<(u64, LogicalInput)> {
    let sample_reports = old_frames
        .iter()
        .take(20)
        .map(|(_, report)| *report)
        .collect::<Vec<_>>();
    let (left_center, right_center) = auto_calibrate_centers(&sample_reports);
    let sticks = StickPair::new(
        StickCalibrator::new(MAIN_STICK_CAL, DEFAULT_DEADZONE),
        StickCalibrator::new(C_STICK_CAL, DEFAULT_DEADZONE),
        left_center,
        right_center,
    );

    old_frames
        .iter()
        .map(|(timestamp_us, report)| {
            let parsed = parse_hid_report(report);
            let (left_stick, right_stick) =
                sticks.calibrate(parsed.left_stick_raw, parsed.right_stick_raw);
            (
                *timestamp_us,
                LogicalInput::from_parts(parsed.buttons, left_stick, right_stick),
            )
        })
        .collect()
}

/// Simple timestamp without pulling in chrono.
fn unix_timestamp_secs() -> String {
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    dur.as_secs().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn frame(ts: u64, button_mask: u32) -> (u64, LogicalInput) {
        (
            ts,
            LogicalInput::from_parts(
                ButtonState::from_mask(button_mask),
                (12.34, -56.78),
                (0.0, 100.0),
            ),
        )
    }

    #[test]
    fn save_macro_writes_mac3_logical_frames() {
        let dir = TempDir::new().unwrap();
        let id = save_macro(dir.path(), &[frame(0, 1), frame(1000, 3)], None).unwrap();
        let entry = get_macro_info(dir.path(), id).unwrap();
        let data = fs::read(dir.path().join(entry.filename)).unwrap();

        assert_eq!(&data[0..4], MAGIC);
        assert_eq!(u16::from_le_bytes(data[4..6].try_into().unwrap()), 3);
        assert_eq!(
            u16::from_le_bytes(data[6..8].try_into().unwrap()),
            FRAME_SIZE as u16
        );
        assert_eq!(u32::from_le_bytes(data[8..12].try_into().unwrap()), 2);

        let (_, decoded) = decode_frame(&data[HEADER_SIZE..HEADER_SIZE + FRAME_SIZE]).unwrap();
        assert_eq!(decoded.buttons.to_mask(), 1);
        assert_eq!(decoded.left_stick, (12.34, -56.78));
        assert_eq!(decoded.right_stick, (0.0, 100.0));
    }

    #[test]
    fn migrate_old_macros_backs_up_and_rewrites_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("001_old.bin");
        let mut report = [0u8; 64];
        report[3] = 0x02; // A in old USB layout
        report[6] = 0x00;
        report[7] = 0x08;
        report[8] = 0x80;
        report[9] = 0x00;
        report[10] = 0x08;
        report[11] = 0x80;

        let mut old = Vec::new();
        old.extend_from_slice(OLD_MAGIC_MAC2);
        old.extend_from_slice(&2u16.to_le_bytes());
        old.extend_from_slice(&OLD_REPORT_SIZE.to_le_bytes());
        old.extend_from_slice(&1u32.to_le_bytes());
        old.extend_from_slice(&0u32.to_le_bytes());
        old.extend_from_slice(&0u64.to_le_bytes());
        old.extend_from_slice(&report);
        fs::write(&path, old).unwrap();
        save_index(
            dir.path(),
            &[MacroEntry {
                id: 1,
                name: "old".into(),
                filename: "001_old.bin".into(),
                frame_count: 1,
                duration_ms: 0,
                created: "0".into(),
            }],
        );

        let backup = migrate_old_macros(dir.path()).unwrap().unwrap();
        assert!(backup.join("001_old.bin").exists());

        let migrated = fs::read(&path).unwrap();
        assert_eq!(&migrated[0..4], MAGIC);
        let (_, input) = decode_frame(&migrated[HEADER_SIZE..HEADER_SIZE + FRAME_SIZE]).unwrap();
        assert_eq!(
            input.buttons.to_mask(),
            ButtonState::from_bytes([0x02, 0, 0]).to_mask()
        );
    }
}
