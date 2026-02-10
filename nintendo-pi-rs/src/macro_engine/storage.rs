//! Macro storage: binary format + JSON index CRUD.
//!
//! Binary format (MAC2):
//!   Header (16 bytes):
//!     [0..4]   Magic "MAC2"
//!     [4..6]   Version (u16 LE) = 2
//!     [6..8]   Report size (u16 LE) = 64
//!     [8..12]  Frame count (u32 LE)
//!     [12..16] Duration microseconds (u32 LE)
//!
//!   Per frame (72 bytes):
//!     [0..8]   Timestamp microseconds (u64 LE)
//!     [8..72]  Raw 64-byte HID report

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{error, info};

pub const MAGIC: &[u8; 4] = b"MAC2";
pub const FORMAT_VERSION: u16 = 2;
pub const REPORT_SIZE: u16 = 64;
pub const HEADER_SIZE: usize = 16;
pub const FRAME_SIZE: usize = 8 + REPORT_SIZE as usize; // 72

/// Also support reading Python's "MACO" v1 format (identical layout).
pub const MAGIC_V1: &[u8; 4] = b"MACO";

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

/// Save recorded frames to a binary file and update the index.
/// Returns the macro ID.
pub fn save_macro(
    macros_dir: &Path,
    frames: &[(u64, [u8; 64])],
    name: Option<&str>,
) -> Option<u32> {
    if frames.is_empty() {
        return None;
    }

    fs::create_dir_all(macros_dir).ok();
    let mut index = load_index(macros_dir);
    let id = next_id(&index);
    let default_name = format!("macro_{id}");
    let name = name.unwrap_or(&default_name).to_string();
    let filename = format!("{id:03}_{name}.bin");
    let filepath = macros_dir.join(&filename);

    let frame_count = frames.len() as u32;
    let duration_us = frames.last().map(|(ts, _)| *ts as u32).unwrap_or(0);

    // Write binary file
    let mut data = Vec::with_capacity(HEADER_SIZE + frames.len() * FRAME_SIZE);

    // Header
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    data.extend_from_slice(&REPORT_SIZE.to_le_bytes());
    data.extend_from_slice(&frame_count.to_le_bytes());
    data.extend_from_slice(&duration_us.to_le_bytes());

    // Frames
    for (ts, report) in frames {
        data.extend_from_slice(&ts.to_le_bytes());
        data.extend_from_slice(report);
    }

    if let Err(e) = fs::write(&filepath, &data) {
        error!("[MACRO] Failed to write macro file: {e}");
        return None;
    }

    let entry = MacroEntry {
        id,
        name,
        filename,
        frame_count,
        duration_ms: duration_us / 1000,
        created: chrono_now(),
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
        let old_path = macros_dir.join(&entry.filename);
        let new_filename = format!("{:03}_{}.bin", macro_id, new_name);
        let new_path = macros_dir.join(&new_filename);
        if old_path.exists() {
            let _ = fs::rename(&old_path, &new_path);
        }
        entry.name = new_name.to_string();
        entry.filename = new_filename;
        save_index(macros_dir, &index);
        true
    } else {
        false
    }
}

pub fn delete_macro(macros_dir: &Path, macro_id: u32) -> bool {
    let mut index = load_index(macros_dir);
    let orig_len = index.len();

    index.retain(|entry| {
        if entry.id == macro_id {
            let _ = fs::remove_file(macros_dir.join(&entry.filename));
            false
        } else {
            true
        }
    });

    let deleted = index.len() < orig_len;
    if deleted {
        save_index(macros_dir, &index);
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

/// Simple timestamp without pulling in chrono.
fn chrono_now() -> String {
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // Format as ISO-ish date (good enough for display)
    format!("{secs}")
}
